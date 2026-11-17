//! Blockchain CLI commands for mAgent.
//!
//! This module extends the `magent web3` subcommand with blockchain-specific
//! operations. It is the executor behind the `chain` / `bind` / `balance` /
//! `tx` / `vc` actions that [`crate::cli`] parses:
//!
//! - **Chain Management**: enumerate and describe the chains this build knows
//! - **Identity Binding**: prove control of an address, and verify the proof
//! - **Balance Checking**: query a native-token balance over JSON-RPC
//! - **Transaction Building**: build an unsigned transaction request
//! - **Credentials**: issue, verify and present verifiable credentials
//!
//! ## Subcommands
//!
//! ```text
//! magent web3 chain list                             List supported chains
//! magent web3 chain info <CHAIN>                     Show chain configuration
//! magent web3 balance <ADDRESS> [--chain <CHAIN>]    Query a native balance
//! magent web3 tx build <TO> --value <ETH> [--data <HEX>] [--chain <CHAIN>]
//!                                                    Build an unsigned transaction
//! magent web3 bind create <DID> --chain <CHAIN> --from-seed <HEX>
//!                                                    Sign a DID→address binding proof
//! magent web3 bind verify <FILE>                     Verify a binding proof
//! magent web3 vc issue <SUBJECT_DID> --type <TYPE> [--claim <KEY>=<JSON>]...
//!                                                    Issue and sign a credential
//! magent web3 vc verify <FILE>                       Check a credential
//! magent web3 vc present <FILE> --challenge <STR>    Build a signed presentation
//! ```
//!
//! Keys come from `--from-seed <HEX>` (a raw 32-byte hex seed, so the commands
//! are scriptable and testable) or, for the Ed25519 credential commands, from
//! a named identity in the vault (`--issuer <NAME>` / `--holder <NAME>`). The
//! binding commands need a secp256k1 key — an Ethereum address is what the
//! proof recovers to — so they take a seed, not a vault name: the vault stores
//! Ed25519 identities and cannot produce an Ethereum signature.
//!
//! `balance` and `bind create`'s on-chain submission are the only parts that
//! need the network; everything else runs offline.

#![cfg(feature = "blockchain")]

use std::path::{Path, PathBuf};
use std::string::{String, ToString};
use std::vec::Vec;

use serde::Serialize;

use magent_core::web3 as core;
use magent_core::web3::blockchain::identity_binding::{
    BindingClaim, BindingProof, IdentityBinding,
};
use magent_core::web3::blockchain::secp256k1::{Secp256k1Keypair, TransactionSigner};
use magent_core::web3::blockchain::{
    Address, Chain, ChainClient, HttpRpcClient, KnownChain, TransactionRequest, Wei,
};
use magent_core::web3::verifiable_credentials::PresentationProof;
use magent_core::web3::{
    CredentialProof, CredentialSubject, ProofType, VerifiableCredential, VerifiablePresentation,
};

use crate::output::{Output, OutputKind};
use crate::web3::{PassphraseResolver, Web3CliError};

/// Blockchain-related subcommands.
#[derive(Debug, Clone)]
pub enum BlockchainAction {
    /// `magent web3 chain list`
    ChainList,
    /// `magent web3 chain info <CHAIN>`
    ChainInfo(String),
    /// `magent web3 balance <ADDRESS>`
    Balance(BalanceOptions),
    /// `magent web3 tx build <TO>`
    TxBuild(TxBuildOptions),
    /// `magent web3 bind create <DID>`
    BindCreate(BindCreateOptions),
    /// `magent web3 bind verify <FILE>`
    BindVerify(PathBuf),
    /// `magent web3 vc issue <SUBJECT_DID>`
    VcIssue(VcIssueOptions),
    /// `magent web3 vc verify <FILE>`
    VcVerify(PathBuf),
    /// `magent web3 vc present <FILE>`
    VcPresent(VcPresentOptions),
    /// `magent web3 vc verify-presentation <FILE> [--challenge <STR>]`
    ///
    /// `vc present` could sign a presentation but nothing could check one, so
    /// a tampered or unsigned presentation looked the same as a valid one.
    /// `--challenge` supplies the verifier's nonce: when given, the
    /// presentation must carry exactly that challenge, which is what makes a
    /// signature non-replayable to another verifier.
    VcVerifyPresentation {
        /// Presentation file to verify.
        path: PathBuf,
        /// The challenge this verifier issued, if it issued one.
        challenge: Option<String>,
    },
}

/// Where a signing key comes from.
#[derive(Debug, Clone)]
pub enum KeySource {
    /// A raw 32-byte seed, hex-encoded.
    Seed(String),
    /// A named identity in the vault.
    Vault {
        name: String,
        passphrase_env: Option<String>,
        vault: Option<PathBuf>,
    },
}

/// Options for binding creation.
///
/// The bound address is derived from `from_seed` rather than supplied: the
/// proof is only meaningful if it recovers to the address it claims, so
/// letting the two disagree would produce an artifact that never verifies.
#[derive(Debug, Clone)]
pub struct BindCreateOptions {
    pub did: String,
    pub chain: String,
    pub from_seed: String,
    pub expiry_days: Option<u64>,
    pub output: Option<PathBuf>,
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
    pub issuer: KeySource,
    /// `--valid-until <ISO8601>` — passed through verbatim; the credential's
    /// own `is_expired` parses it to an instant (a timezone offset is applied,
    /// and an unparseable bound counts as expired).
    pub valid_until: Option<String>,
    pub output: Option<PathBuf>,
}

/// Options for VC presentation.
#[derive(Debug, Clone)]
pub struct VcPresentOptions {
    pub vc_file: PathBuf,
    pub holder: KeySource,
    pub challenge: String,
    pub domain: Option<String>,
    pub output: Option<PathBuf>,
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
    /// Unix seconds the binding proof was created.
    pub created_at: u64,
    /// Optional expiry (Unix seconds). Serialised as `null` by
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
pub fn render_chain_list(chains: Vec<ChainDisplay>, out: &mut Output) -> Result<(), Web3CliError> {
    if out.kind() == OutputKind::Json {
        render_json(&chains, out)?;
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
    Ok(())
}

/// Render chain info output.
pub fn render_chain_info(chain: &ChainDisplay, out: &mut Output) -> Result<(), Web3CliError> {
    if out.kind() == OutputKind::Json {
        render_json(chain, out)?;
    } else {
        let _ = out.info(&format!("Chain: {}", chain.name));
        let _ = out.info(&format!("  ID: {}", chain.id));
        let _ = out.info(&format!("  Currency: {}", chain.currency));
        let _ = out.info(&format!(
            "  RPC: {}",
            if chain.has_rpc {
                "available"
            } else {
                "not configured"
            }
        ));
        let _ = out.info(&format!(
            "  Explorer: {}",
            chain.explorer.as_deref().unwrap_or("-")
        ));
    }
    Ok(())
}

/// Render balance output.
pub fn render_balance(balance: &BalanceDisplay, out: &mut Output) -> Result<(), Web3CliError> {
    if out.kind() == OutputKind::Json {
        render_json(balance, out)?;
    } else {
        let _ = out.info(&format!("Address: {}", balance.address));
        let _ = out.info(&format!("Chain: {}", balance.chain));
        let _ = out.info(&format!("Balance: {} ETH", balance.balance_ether));
        let _ = out.info(&format!("         {} wei", balance.balance_wei));
    }
    Ok(())
}

/// Render transaction output.
pub fn render_tx(tx: &TxDisplay, out: &mut Output) -> Result<(), Web3CliError> {
    if out.kind() == OutputKind::Json {
        render_json(tx, out)?;
    } else {
        let _ = out.info("Transaction request:");
        let _ = out.info(&format!("  To: {}", tx.to));
        let _ = out.info(&format!("  Value: {} ETH", tx.value_ether));
        let _ = out.info(&format!("  Data: {} bytes", tx.data_length));
        let _ = out.info(&format!("  Encoded: {} bytes", tx.encoded_length));
        let _ = out.info("");
        let _ = out.info("Note: This is an unsigned transaction. Sign and broadcast separately.");
    }
    Ok(())
}

/// Render binding output.
pub fn render_binding(binding: &BindingDisplay, out: &mut Output) -> Result<(), Web3CliError> {
    if out.kind() == OutputKind::Json {
        render_json(binding, out)?;
    } else {
        let _ = out.info("Identity Binding:");
        let _ = out.info(&format!("  DID: {}", binding.did));
        let _ = out.info(&format!("  Address: {}", binding.address));
        let _ = out.info(&format!("  Chain: {}", binding.chain));
        let _ = out.info(&format!("  Status: {}", binding.status));
        let _ = out.info(&format!("  Created: {} (unix)", binding.created_at));
        if let Some(expires) = binding.expires_at {
            let _ = out.info(&format!("  Expires: {} (unix)", expires));
        }
    }
    Ok(())
}

/// Render binding verification result.
pub fn render_binding_verify(
    is_valid: bool,
    binding: Option<&BindingDisplay>,
    out: &mut Output,
) -> Result<(), Web3CliError> {
    if out.kind() == OutputKind::Json {
        let json = serde_json::json!({
            "is_valid": is_valid,
            "binding": binding
        });
        render_json(&json, out)?;
    } else {
        if is_valid {
            let _ = out.info("✓ Binding is valid");
            if let Some(b) = binding {
                let _ = out.info(&format!("  DID: {}", b.did));
                let _ = out.info(&format!("  Address: {}", b.address));
                let _ = out.info(&format!("  Chain: {}", b.chain));
            }
            let _ = out.info(
                "  note: this verifies the signed proof only — it does not confirm an on-chain registration",
            );
        } else {
            let _ = out.info("✗ Binding is invalid or not found");
        }
    }
    Ok(())
}

/// Render VC output.
pub fn render_vc(vc: &VcDisplay, out: &mut Output) -> Result<(), Web3CliError> {
    if out.kind() == OutputKind::Json {
        render_json(vc, out)?;
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
    Ok(())
}

/// Render VC verification result.
pub fn render_vc_verify(
    is_valid: bool,
    vc: Option<&VcDisplay>,
    out: &mut Output,
) -> Result<(), Web3CliError> {
    if out.kind() == OutputKind::Json {
        let json = serde_json::json!({
            "is_valid": is_valid,
            "credential": vc
        });
        render_json(&json, out)?;
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
    Ok(())
}

/// Render presentation output.
pub fn render_presentation(vp: &PresentationDisplay, out: &mut Output) -> Result<(), Web3CliError> {
    if out.kind() == OutputKind::Json {
        render_json(vp, out)?;
    } else {
        let _ = out.info("Verifiable Presentation:");
        let _ = out.info(&format!("  ID: {}", vp.id));
        let _ = out.info(&format!("  Holder: {}", vp.holder));
        let _ = out.info(&format!("  Credentials: {}", vp.credential_count));
        let _ = out.info(&format!("  Created: {}", vp.created));
        let proof_status = if vp.has_proof { "signed" } else { "unsigned" };
        let _ = out.info(&format!("  Proof: {}", proof_status));
    }
    Ok(())
}

/// Serialise a display record for the JSON path.
///
/// A display record is built from already-validated data (hex strings,
/// decimal numbers, unit-tagged integers), so serialisation cannot
/// fail in practice. If it somehow does, the fallback is a *reported*
/// error rather than an empty payload: the previous
/// `unwrap_or_default()` made the command print a bare newline and
/// still exit 0, which a `| jq` consumer read as "no result".
fn render_json<T: Serialize>(value: &T, out: &mut Output) -> Result<(), Web3CliError> {
    match serde_json::to_string(value) {
        Ok(json) => {
            out.write_json_str(json);
            Ok(())
        }
        Err(e) => Err(Web3CliError::Serialise(e.to_string())),
    }
}

// ============================================================================
// Executor
// ============================================================================

/// Run one blockchain action and render its result to `out`.
pub fn execute(
    action: &BlockchainAction,
    passphrase: &mut PassphraseResolver,
    out: &mut Output,
) -> Result<(), Web3CliError> {
    match action {
        BlockchainAction::ChainList => {
            let chains: Vec<ChainDisplay> =
                KnownChain::ALL.iter().copied().map(chain_display).collect();
            render_chain_list(chains, out)?;
            Ok(())
        }
        BlockchainAction::ChainInfo(name) => {
            let chain = resolve_chain(name)?;
            render_chain_info(&chain_display(chain), out)?;
            Ok(())
        }
        BlockchainAction::Balance(opts) => run_balance(opts, out),
        BlockchainAction::TxBuild(opts) => run_tx_build(opts, out),
        BlockchainAction::BindCreate(opts) => run_bind_create(opts, out),
        BlockchainAction::BindVerify(path) => run_bind_verify(path, out),
        BlockchainAction::VcIssue(opts) => run_vc_issue(opts, passphrase, out),
        BlockchainAction::VcVerify(path) => run_vc_verify(path, out),
        BlockchainAction::VcPresent(opts) => run_vc_present(opts, passphrase, out),
        BlockchainAction::VcVerifyPresentation { path, challenge } => {
            run_vc_verify_presentation(path, challenge.as_deref(), out)
        }
    }
}

fn chain_display(chain: KnownChain) -> ChainDisplay {
    ChainDisplay {
        id: chain.chain_id(),
        name: chain.name().to_string(),
        currency: chain.currency_symbol().to_string(),
        has_rpc: chain.public_rpc().is_some(),
        explorer: chain.block_explorer().map(str::to_string),
    }
}

fn resolve_chain(name: &str) -> Result<KnownChain, Web3CliError> {
    KnownChain::from_name(name).map_err(Web3CliError::Core)
}

fn run_balance(opts: &BalanceOptions, out: &mut Output) -> Result<(), Web3CliError> {
    let chain = resolve_chain(opts.chain.as_deref().unwrap_or("ethereum"))?;
    let address = Address::from_hex(&opts.address).map_err(Web3CliError::Core)?;
    let client = HttpRpcClient::from_chain(Chain::from_known(chain).config);
    let wei = client.get_balance(&address).map_err(Web3CliError::Core)?;
    let display = BalanceDisplay {
        address: address.to_checksum(),
        chain: chain.name().to_string(),
        balance_wei: wei.as_wei().to_string(),
        balance_ether: format!("{:.6}", wei.as_ether()),
    };
    render_balance(&display, out)?;
    Ok(())
}

fn run_tx_build(opts: &TxBuildOptions, out: &mut Output) -> Result<(), Web3CliError> {
    let chain = resolve_chain(&opts.chain)?;
    let to = Address::from_hex(&opts.to).map_err(Web3CliError::Core)?;
    let value = match opts.value.as_deref() {
        Some(v) => wei_from_eth(v)?,
        None => Wei::ZERO,
    };
    let data = match opts.data.as_deref() {
        Some(d) => decode_hex(d)?,
        None => Vec::new(),
    };
    let request = TransactionRequest::new(Some(to), value, data.clone(), chain.chain_id());
    let display = TxDisplay {
        to: to.to_checksum(),
        value_wei: value.as_wei().to_string(),
        value_ether: format!("{:.6}", value.as_ether()),
        data_length: data.len(),
        encoded_length: request.encode_for_signing().len(),
    };
    render_tx(&display, out)?;
    Ok(())
}

fn run_bind_create(opts: &BindCreateOptions, out: &mut Output) -> Result<(), Web3CliError> {
    let chain = resolve_chain(&opts.chain)?;
    let keypair = Secp256k1Keypair::from_hex(&opts.from_seed).map_err(Web3CliError::Core)?;
    let address = *keypair.address();
    let validity_seconds = opts.expiry_days.unwrap_or(365).saturating_mul(86_400);
    let claim = BindingClaim::new(
        opts.did.clone(),
        address,
        chain.chain_id(),
        validity_seconds,
    );
    let message = claim.encode();
    let signature = TransactionSigner::sign_personal_message(keypair.secret_key(), &message)
        .map_err(Web3CliError::Core)?;
    let proof = BindingProof::new(
        opts.did.clone(),
        address,
        chain.chain_id(),
        signature.to_hex(),
        // `BindingClaim::encode` emits ASCII, so this lossy conversion is
        // byte-exact and the verifier signs the same bytes we did.
        String::from_utf8_lossy(&message).into_owned(),
    );
    // Never hand back an artifact we have not checked: a proof that does not
    // verify is worse than no proof, because it looks like one.
    match proof.verify() {
        Ok(true) => {}
        Ok(false) => {
            return Err(Web3CliError::Usage(
                "internal error: the binding proof we just signed does not verify".to_string(),
            ))
        }
        Err(e) => return Err(Web3CliError::Core(e)),
    }

    let now = now_unix();
    let binding = IdentityBinding::new(opts.did.clone(), address, chain.chain_id(), 0, now)
        .with_expiry(claim.expires_at);
    let document = serde_json::json!({ "binding": binding, "proof": proof });
    // A serialisation failure must be reported, not turned into an empty
    // document: `unwrap_or_default()` would write a zero-byte "binding proof"
    // and still exit 0.
    let text = serde_json::to_string_pretty(&document)
        .map_err(|e| Web3CliError::Serialise(e.to_string()))?;
    let echoed = emit(&text, opts.output.as_deref(), out, "binding proof")?;

    let display = BindingDisplay {
        did: opts.did.clone(),
        address: address.to_checksum(),
        chain: chain.name().to_string(),
        // Honest: the proof is signed and locally verified, but nothing has
        // been broadcast, so it is not a confirmed on-chain binding.
        status: "Proof signed (not submitted on-chain)".to_string(),
        created_at: now,
        expires_at: Some(claim.expires_at),
    };
    if !echoed {
        render_binding(&display, out)?;
    }
    Ok(())
}

fn run_bind_verify(path: &Path, out: &mut Output) -> Result<(), Web3CliError> {
    let text = read_file(path)?;
    let value: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| Web3CliError::Usage(format!("{} is not valid JSON: {e}", path.display())))?;
    // Accept either a bare `BindingProof` or the `{binding, proof}` envelope
    // `bind create` writes, so the two commands round-trip.
    let proof_value = value.get("proof").cloned().unwrap_or_else(|| value.clone());
    let proof: BindingProof = serde_json::from_value(proof_value).map_err(|e| {
        Web3CliError::Usage(format!("{} is not a binding proof: {e}", path.display()))
    })?;

    // The signature is necessary but not sufficient. `BindingProof::verify`
    // only recovers the signer of `message`; the proof's own `did` /
    // `address` / `chain_id` are metadata that nothing binds to the signed
    // bytes, so editing the DID alone would still report "valid". Check the
    // metadata against the claim inside the signed message too.
    let signature_ok = proof.verify().map_err(Web3CliError::Core)?;
    let metadata_ok = message_binds_metadata(&proof);
    // A binding carries an expiry. `bind create` signs `expiresAt` into the
    // message, so the signature still verifies long after that instant —
    // without this check `bind verify` reported a one-day binding as valid a
    // year later, and a CI gate that relies on it could not fail.
    let not_expired = !claim_expired(&proof.message, now_unix());
    // When the file is the full envelope, the `binding` record has to agree
    // with the proof for the same reason.
    let envelope_ok = match value.get("binding") {
        Some(binding) => serde_json::from_value::<IdentityBinding>(binding.clone())
            .map(|b| {
                b.did == proof.did
                    && b.address.to_hex() == proof.address.to_hex()
                    && b.chain_id == proof.chain_id
            })
            .unwrap_or(false),
        None => true,
    };

    // A failed verification must be a failed *command*: rendering `✗` and
    // returning Ok made `bind verify` exit 0 on a tampered proof, the opposite
    // of `web3 verify` and unusable as a CI gate.
    if !(signature_ok && metadata_ok && not_expired && envelope_ok) {
        let reason = if !not_expired {
            format!(
                "{}: binding proof has expired (its signed expiresAt is in the past)",
                path.display()
            )
        } else {
            format!(
                "{}: binding proof is invalid or its metadata is not bound to \
                 the signed message",
                path.display()
            )
        };
        return Err(Web3CliError::VerificationFailed { reason });
    }
    render_binding_verify(true, None, out)?;
    Ok(())
}

/// Confirm the proof's `did`, `address` and `chain_id` are the ones encoded
/// in its signed `message`.
///
/// `BindingClaim::encode` writes `did:{did}` / `address:{addr}` /
/// `chainId:{id}` on their own lines. The DID contains colons itself
/// (`did:key:…`), but the field prefix is the first colon, so splitting each
/// line once recovers the field and its whole value.
fn message_binds_metadata(proof: &BindingProof) -> bool {
    let mut fields = std::collections::HashMap::new();
    for line in proof.message.lines() {
        if let Some((key, value)) = line.split_once(':') {
            fields.insert(key, value);
        }
    }
    let address = proof.address.to_hex();
    let chain_id = proof.chain_id.to_string();
    fields.get("did").is_some_and(|v| *v == proof.did.as_str())
        && fields
            .get("address")
            .is_some_and(|v| *v == address.as_str())
        && fields
            .get("chainId")
            .is_some_and(|v| *v == chain_id.as_str())
}

/// Whether the signed `expiresAt:` field of a binding message is in the past.
///
/// `BindingClaim::encode` writes `expiresAt:{unix_secs}` as the last line, so
/// the value is part of the signed bytes and cannot be edited — but the
/// signature says "the holder signed this claim", not "this claim is still
/// current". A missing or unparseable field means the claim cannot be shown
/// to be in force, so it is treated as expired (fail closed), matching
/// `VerifiableCredential::is_expired`.
fn claim_expired(message: &str, now: u64) -> bool {
    message
        .lines()
        .find_map(|line| line.strip_prefix("expiresAt:"))
        .and_then(|v| v.trim().parse::<u64>().ok())
        .is_none_or(|expires_at| now > expires_at)
}

fn run_vc_issue(
    opts: &VcIssueOptions,
    passphrase: &mut PassphraseResolver,
    out: &mut Output,
) -> Result<(), Web3CliError> {
    let issuer = identity_from_key_source(&opts.issuer, passphrase)?;
    let issuer_did = issuer.did_key().as_str().to_string();

    // `try_with_claim` so a claim name that collides with a
    // `credentialSubject` field is an error here rather than a silently
    // missing claim (or, worse, a duplicate JSON key that no reader in this
    // repo can parse back — see `is_reserved_claim_key`).
    let mut subject = CredentialSubject::new(opts.subject_did.clone());
    for (key, value) in &opts.claims {
        subject = subject
            .try_with_claim(key.clone(), value)
            .map_err(Web3CliError::Core)?;
    }
    let types = vec![
        "VerifiableCredential".to_string(),
        opts.credential_type.clone(),
    ];
    let mut credential = VerifiableCredential::new(new_id(), issuer_did.clone(), types, subject);
    if let Some(until) = &opts.valid_until {
        // A bound the verifier cannot evaluate makes `is_expired` answer
        // "expired", so a typo (`2027-13-01`, a missing `T`, a stray word)
        // would sign and write a credential that `vc verify` rejects forever
        // — while this command printed success. Refuse it at the issuer.
        if !magent_core::web3::is_valid_instant_bound(until) {
            return Err(Web3CliError::Usage(format!(
                "--valid-until {until:?} is not a valid ISO-8601 instant \
                 (expected e.g. 2027-01-01T00:00:00Z); a credential issued \
                 with it could never verify"
            )));
        }
        credential = credential.with_expiry(until.clone());
    }

    // Build the proof first, hash the body with its options, then attach the
    // signature — the options are part of the signed bytes.
    let mut proof = CredentialProof::new(
        ProofType::Ed25519Signature2020,
        verification_method(&issuer_did),
        core::Signature::from_bytes(&[0u8; 64])?,
    );
    let bytes = credential
        .signing_input(&proof)
        .map_err(Web3CliError::Core)?;
    proof.proof_value = sign_ed25519(&issuer, &bytes)?.to_hex();
    credential.sign(proof);
    credential.validate().map_err(Web3CliError::Core)?;

    let text = String::from_utf8_lossy(&credential.encode_canonical()).into_owned();
    let echoed = emit(&text, opts.output.as_deref(), out, "credential")?;
    if !echoed {
        render_vc(&vc_display(&credential), out)?;
    }
    Ok(())
}

fn run_vc_verify(path: &Path, out: &mut Output) -> Result<(), Web3CliError> {
    let text = read_file(path)?;
    let credential: VerifiableCredential = serde_json::from_str(&text).map_err(|e| {
        Web3CliError::Usage(format!(
            "{} is not a verifiable credential: {e}",
            path.display()
        ))
    })?;
    // Structural validity, expiry, and the Ed25519 proof. The issuer is a
    // `did:key`, so the public key is embedded in the proof's verification
    // method — the old note claiming it was unavailable was wrong, and
    // skipping the check let a tampered subject pass as valid.
    //
    // `is_not_yet_valid` is part of the same check the core `verify()` makes
    // for a credential inside a presentation. This path assembled the checks
    // by hand and omitted it, so a credential stamped `validFrom: 2999-…`
    // passed `vc verify` (exit 0, "valid") while the same credential inside a
    // presentation was rejected — two entry points, opposite verdicts.
    if credential.is_not_yet_valid() {
        return Err(Web3CliError::VerificationFailed {
            reason: format!(
                "{}: credential is not yet valid (validFrom is in the future \
                 or unparseable)",
                path.display()
            ),
        });
    }
    if credential.is_expired() {
        return Err(Web3CliError::VerificationFailed {
            reason: format!("{}: credential has expired", path.display()),
        });
    }
    credential.validate().map_err(Web3CliError::Core)?;
    credential
        .verify_proof()
        .map_err(|e| Web3CliError::VerificationFailed {
            reason: format!("{}: {e}", path.display()),
        })?;
    render_vc_verify(true, Some(&vc_display(&credential)), out)?;
    Ok(())
}

fn run_vc_verify_presentation(
    path: &Path,
    expected_challenge: Option<&str>,
    out: &mut Output,
) -> Result<(), Web3CliError> {
    let text = read_file(path)?;
    let presentation: VerifiablePresentation = serde_json::from_str(&text).map_err(|e| {
        Web3CliError::Usage(format!(
            "{} is not a verifiable presentation: {e}",
            path.display()
        ))
    })?;

    // `verify()` — not `validate()` + `verify_proof()`. The outer proof only
    // attests that this holder assembled the document; it says nothing about
    // the credentials inside, which a holder can choose freely. Verifying the
    // outer proof alone accepted a presentation whose only credential named a
    // fake issuer and carried no signature.
    //
    // With `--challenge`, also require that this presentation carries the
    // nonce *this* verifier issued — a presentation signed for somebody else
    // is a valid document that must not be accepted here.
    let outcome = match expected_challenge {
        Some(expected) => presentation.verify_with_challenge(expected),
        None => presentation.verify(),
    };
    outcome.map_err(|e| Web3CliError::VerificationFailed {
        reason: format!("{}: {e}", path.display()),
    })?;

    if out.kind() == OutputKind::Json {
        let json = serde_json::json!({
            "is_valid": true,
            "presentation": {
                "id": presentation.id,
                "holder": presentation.holder,
                "credential_count": presentation.verifiable_credential.len(),
                "has_proof": presentation.proof.is_some(),
                "challenge_checked": expected_challenge.is_some(),
            }
        });
        render_json(&json, out)?;
    } else {
        let _ = out.info("✓ Presentation is valid");
        let _ = out.info(&format!("  Holder: {}", presentation.holder));
        let _ = out.info(&format!(
            "  Credentials: {}",
            presentation.verifiable_credential.len()
        ));
        if expected_challenge.is_some() {
            let _ = out.info("  Challenge: matches the expected value");
        }
    }
    Ok(())
}

fn run_vc_present(
    opts: &VcPresentOptions,
    passphrase: &mut PassphraseResolver,
    out: &mut Output,
) -> Result<(), Web3CliError> {
    let text = read_file(&opts.vc_file)?;
    let credential: VerifiableCredential = serde_json::from_str(&text).map_err(|e| {
        Web3CliError::Usage(format!(
            "{} is not a verifiable credential: {e}",
            opts.vc_file.display()
        ))
    })?;
    let holder = identity_from_key_source(&opts.holder, passphrase)?;
    let holder_did = holder.did_key().as_str().to_string();
    let mut presentation = VerifiablePresentation::new(holder_did.clone(), vec![credential]);

    // Build the proof first, hash the body together with its options through
    // the core helper, then attach the signature. The options (challenge,
    // domain, verification method) must be in the signed bytes — signing a
    // proof-less clone left them unprotected, so a rewritten challenge still
    // verified.
    let mut proof = PresentationProof::new(
        ProofType::Ed25519Signature2020,
        verification_method(&holder_did),
        opts.challenge.clone(),
        // Placeholder, replaced once the options are fixed.
        core::Signature::from_bytes(&[0u8; 64])?,
    );
    if let Some(domain) = &opts.domain {
        proof = proof.with_domain(domain.clone());
    }
    let bytes = presentation
        .signing_input(&proof)
        .map_err(Web3CliError::Core)?;
    proof.proof_value = sign_ed25519(&holder, &bytes)?.to_hex();
    presentation.sign(proof);

    let text = serde_json::to_string_pretty(&presentation)
        .map_err(|e| Web3CliError::Serialise(e.to_string()))?;
    let echoed = emit(&text, opts.output.as_deref(), out, "presentation")?;
    let display = PresentationDisplay {
        id: presentation.id.clone(),
        holder: presentation.holder.clone(),
        credential_count: presentation.verifiable_credential.len(),
        created: presentation.created.clone(),
        has_proof: presentation.proof.is_some(),
    };
    if !echoed {
        render_presentation(&display, out)?;
    }
    Ok(())
}

// ============================================================================
// Helpers
// ============================================================================

fn vc_display(credential: &VerifiableCredential) -> VcDisplay {
    VcDisplay {
        id: credential.id.clone(),
        issuer: credential.issuer.clone(),
        subject: credential.credential_subject.id.clone(),
        credential_type: credential
            .credential_type
            .iter()
            .find(|t| t.as_str() != "VerifiableCredential")
            .cloned()
            .unwrap_or_else(|| credential.credential_type.join(",")),
        valid_from: credential.valid_from.clone(),
        valid_until: credential.valid_until.clone(),
        has_proof: credential.proof.is_some(),
    }
}

/// Sign `payload` with an Ed25519 identity, returning the bare
/// [`core::Signature`] a `CredentialProof` / `PresentationProof` carries.
///
/// `Identity::sign` returns a full `SignedMessage` (payload + signature); the
/// signing key itself is not exposed, so the signature is recovered from the
/// envelope rather than reached for directly.
fn sign_ed25519(
    identity: &core::Identity,
    payload: &[u8],
) -> Result<core::Signature, Web3CliError> {
    let signed = identity.sign(payload).map_err(Web3CliError::Core)?;
    let bytes = decode_hex(&signed.signature_hex)?;
    core::Signature::from_bytes(&bytes).map_err(Web3CliError::Core)
}

/// The `verificationMethod` URL for a `did:key` issuer/holder.
fn verification_method(did: &str) -> String {
    let key = did.strip_prefix("did:key:").unwrap_or(did);
    format!("{did}#{key}")
}

fn identity_from_key_source(
    source: &KeySource,
    passphrase: &mut PassphraseResolver,
) -> Result<core::Identity, Web3CliError> {
    match source {
        KeySource::Seed(hex) => {
            let bytes = decode_hex(hex)?;
            core::Identity::from_secret_bytes(&bytes).map_err(Web3CliError::Core)
        }
        KeySource::Vault {
            name,
            passphrase_env,
            vault,
        } => {
            let path = vault
                .clone()
                .unwrap_or_else(crate::web3::default_vault_path);
            let vault = crate::web3::load_vault(&path)?;
            let entry = vault
                .identities
                .get(name)
                .ok_or_else(|| Web3CliError::NotFound(name.clone()))?;
            let secret = (passphrase)(
                passphrase_env
                    .as_deref()
                    .unwrap_or("MAGENT_WEB3_PASSPHRASE"),
            )?;
            let bytes = crate::web3::decrypt_secret(
                &entry.salt_b64,
                &entry.nonce_b64,
                &entry.ciphertext_b64,
                secret.as_bytes(),
            )?;
            let hex = std::str::from_utf8(&bytes).map_err(|_| {
                Web3CliError::Aead("decrypted seed is not valid UTF-8 (corrupt vault?)".to_string())
            })?;
            core::Identity::from_secret_hex(hex).map_err(Web3CliError::Core)
        }
    }
}

/// Parse a decimal ether amount (`"1"`, `"0.05"`) into wei.
fn wei_from_eth(input: &str) -> Result<Wei, Web3CliError> {
    let input = input.trim();
    let (whole, fraction) = match input.split_once('.') {
        Some((w, f)) => (w, f),
        None => (input, ""),
    };
    if fraction.len() > 18 {
        return Err(Web3CliError::Usage(format!(
            "--value {input:?} has more than 18 decimal places"
        )));
    }
    let bad = || Web3CliError::Usage(format!("--value {input:?} is not a decimal ether amount"));
    let whole: u128 = if whole.is_empty() {
        0
    } else {
        whole.parse().map_err(|_| bad())?
    };
    let mut padded = String::from(fraction);
    while padded.len() < 18 {
        padded.push('0');
    }
    let fraction: u128 = padded.parse().map_err(|_| bad())?;
    let wei = whole
        .checked_mul(1_000_000_000_000_000_000)
        .and_then(|w| w.checked_add(fraction))
        .ok_or_else(|| {
            Web3CliError::Usage(format!("--value {input:?} is too large to express in wei"))
        })?;
    Ok(Wei::from_wei(wei))
}

fn decode_hex(input: &str) -> Result<Vec<u8>, Web3CliError> {
    let hex = input.strip_prefix("0x").unwrap_or(input);
    // Hex is ASCII by definition. Reject anything else before slicing:
    // `&hex[i..i + 2]` panics when a window ends inside a multi-byte
    // character, and "invalid hex digit" would be the wrong thing to say
    // about a string that cannot be hex at all.
    if !hex.is_ascii() {
        return Err(Web3CliError::Usage(format!(
            "hex string {input:?} contains non-ASCII characters"
        )));
    }
    if !hex.len().is_multiple_of(2) {
        return Err(Web3CliError::Usage(format!(
            "hex string {input:?} must have an even number of digits"
        )));
    }
    (0..hex.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&hex[i..i + 2], 16)
                .map_err(|_| Web3CliError::Usage(format!("invalid hex digit in {input:?}")))
        })
        .collect()
}

fn read_file(path: &Path) -> Result<String, Web3CliError> {
    std::fs::read_to_string(path).map_err(|e| Web3CliError::PayloadIo {
        path: path.to_path_buf(),
        source: e,
    })
}

/// Write an artifact to `output`, or print it to stdout when no file is given.
///
/// Returns `true` when the artifact went to stdout (no `--output` given), so
/// the caller can skip its own console rendering: the artifact *is* the
/// command's output in that case. This mirrors `web3 sign` — the no-file path
/// must use [`Output::write_stdout_line`], because `write_json_str` is a no-op
/// in human mode (which silently dropped the credential/proof/presentation
/// into nothing) and, in JSON mode, would emit a second object after the
/// caller's own `render_*` call.
fn emit(
    text: &str,
    output: Option<&Path>,
    out: &mut Output,
    label: &str,
) -> Result<bool, Web3CliError> {
    match output {
        Some(path) => {
            std::fs::write(path, text).map_err(|e| Web3CliError::PayloadIo {
                path: path.to_path_buf(),
                source: e,
            })?;
            let _ = out.info(&format!(
                "wrote {label} ({} bytes) to {}",
                text.len(),
                path.display()
            ));
            Ok(false)
        }
        None => {
            out.write_stdout_line(text);
            Ok(true)
        }
    }
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// A unique `urn:uuid:` id for a credential.
///
/// Credential ids must be unique, not secret, so the clock plus a counter is
/// enough and avoids pulling in an RNG. The fields are laid out in the
/// 8-4-4-4-12 shape so the id is a well-formed UUID string rather than a
/// `urn:uuid:` label bolted onto arbitrary hex.
fn new_id() -> String {
    use std::sync::atomic::{AtomicU32, Ordering};
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let counter = u64::from(COUNTER.fetch_add(1, Ordering::Relaxed));
    let seed = nanos ^ (u64::from(std::process::id()) << 17);
    let tail = (counter + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    format!(
        "urn:uuid:{:08x}-{:04x}-{:04x}-{:04x}-{:012x}",
        seed >> 32,
        (seed >> 16) as u16,
        seed as u16,
        (tail >> 48) as u16,
        tail & 0xffff_ffff_ffff,
    )
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    // Only the tests below name these; keeping them at file scope made
    // `cargo build --features web3,blockchain` warn "unused import".
    use crate::web3::Web3CliError;
    use magent_core::error::Web3ErrorKind;

    use super::*;

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

    /// `bind verify` must refuse a binding whose signed `expiresAt` has passed.
    ///
    /// `bind create --expiry-days 1` signs `expiresAt` into the message, but
    /// the signature verifying says only that the holder signed the claim — a
    /// one-day binding reported "valid" a year later, so it could not gate
    /// anything. `claim_expired` reads the field out of the signed bytes.
    #[test]
    fn a_binding_past_its_signed_expiry_is_reported_as_expired() {
        let message = "DIDBindingClaim\ndid:did:key:z6Mk\naddress:ab\nchainId:1\n\
                       issuedAt:1700000000\nexpiresAt:1700086400";
        // 1700086400 is the instant itself; > is the rule, so it is still
        // valid at exactly that second and expired one second later.
        assert!(!claim_expired(message, 1_700_086_400));
        assert!(claim_expired(message, 1_700_086_401));

        // A message with no parseable `expiresAt` cannot be shown to be in
        // force, so it fails closed rather than passing as a fresh binding.
        assert!(claim_expired("DIDBindingClaim\ndid:did:key:z6Mk", 0));
        assert!(claim_expired("DIDBindingClaim\nexpiresAt:not-a-number", 0));
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

    /// Confirm we can read from stdin without panicking on EOF.
    ///
    /// Under an interactive `cargo test`, stdin is the terminal, so a
    /// plain `read` would block forever waiting for input. That would
    /// hang the whole test suite. We guard with `is_terminal()`: when
    /// stdin is a TTY we skip the actual read (a terminal never hits
    /// EOF anyway, so it's not the case the historical panic covered).
    /// When stdin is piped/closed (CI, `< /dev/null`), we perform the
    /// read to exercise the EOF path that used to panic.
    #[test]
    fn stdin_read_does_not_panic_on_eof() {
        use std::io::{IsTerminal, Read};
        if std::io::stdin().is_terminal() {
            return;
        }
        let mut buf = [0u8; 16];
        // We don't care what the result is, only that the API
        // is callable from tests without panicking.
        let _ = std::io::stdin().read(&mut buf);
    }

    // -----------------------------------------------------------------------
    // Round 23: the `chain|balance|tx|bind|vc` commands are implemented
    // offline except for `balance`. The tests below cover the parts that do
    // not touch the network: value parsing, metadata binding, and the
    // bind/vc issue→verify round trips.
    // -----------------------------------------------------------------------

    fn no_passphrase(_: &str) -> Result<String, Web3CliError> {
        Err(Web3CliError::Usage("no passphrase in tests".into()))
    }

    fn tmp_path(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("magent-blockchain-tests-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    /// The seed every offline test signs with.
    const TEST_SEED_HEX: &str = "0000000000000000000000000000000000000000000000000000000000000001";

    #[test]
    fn wei_from_eth_parses_decimal_amounts() {
        assert_eq!(
            wei_from_eth("1").unwrap().as_wei(),
            1_000_000_000_000_000_000
        );
        assert_eq!(
            wei_from_eth("0.05").unwrap().as_wei(),
            50_000_000_000_000_000
        );
        assert_eq!(
            wei_from_eth("1.5").unwrap().as_wei(),
            1_500_000_000_000_000_000
        );
        assert_eq!(wei_from_eth("0").unwrap().as_wei(), 0);
        assert_eq!(
            wei_from_eth(".5").unwrap().as_wei(),
            500_000_000_000_000_000
        );
    }

    #[test]
    fn wei_from_eth_rejects_unrepresentable_amounts() {
        // More precision than wei can hold.
        assert!(wei_from_eth("0.0000000000000000001").is_err());
        // Not a number.
        assert!(wei_from_eth("abc").is_err());
        assert!(wei_from_eth("1.2.3").is_err());
        // Larger than u128 wei.
        assert!(wei_from_eth("999999999999999999999999999999999999999").is_err());
    }

    #[test]
    fn decode_hex_accepts_prefixed_and_bare() {
        assert_eq!(
            decode_hex("0xdeadbeef").unwrap(),
            vec![0xde, 0xad, 0xbe, 0xef]
        );
        assert_eq!(
            decode_hex("DEADBEEF").unwrap(),
            vec![0xde, 0xad, 0xbe, 0xef]
        );
        assert_eq!(decode_hex("").unwrap(), Vec::<u8>::new());
    }

    #[test]
    fn decode_hex_rejects_odd_length_and_bad_digits() {
        assert!(decode_hex("0xabc").is_err());
        assert!(decode_hex("0xzz").is_err());
    }

    /// A hex argument is arbitrary CLI text, so it can contain non-ASCII bytes.
    /// Slicing `&hex[i..i + 2]` on a `&str` requires char boundaries: an
    /// even-length input whose windows straddle a multi-byte character made the
    /// slice panic instead of reporting the bad digit.
    #[test]
    fn decode_hex_rejects_non_ascii_without_panicking() {
        // `€` occupies bytes 2..5, so the window at byte 4 starts inside it.
        let err = decode_hex("11€x").unwrap_err();
        assert!(matches!(err, Web3CliError::Usage(_)), "{err:?}");
        // A multi-byte char at the start (the window ends inside it).
        assert!(decode_hex("€€").is_err());
        // A single multi-byte char is exactly two bytes and still not hex.
        assert!(decode_hex("0xé").is_err());
    }

    #[test]
    fn new_id_is_a_well_formed_uuid_urn() {
        let id = new_id();
        let rest = id.strip_prefix("urn:uuid:").expect("uuid urn prefix");
        let groups: Vec<&str> = rest.split('-').collect();
        assert_eq!(groups.len(), 5, "uuid shape: {rest}");
        assert_eq!(
            groups.iter().map(|g| g.len()).collect::<Vec<_>>(),
            vec![8, 4, 4, 4, 12],
            "uuid group widths: {rest}"
        );
        assert!(rest.chars().all(|c| c.is_ascii_hexdigit() || c == '-'));
        // Two ids from the same process must not collide.
        assert_ne!(new_id(), new_id());
    }

    #[test]
    fn message_binds_metadata_detects_a_tampered_did() {
        let keypair = Secp256k1Keypair::from_hex(TEST_SEED_HEX).unwrap();
        let address = *keypair.address();
        let chain_id = 1u64;
        let claim = BindingClaim::new("did:key:z6MkHonest".to_string(), address, chain_id, 86_400);
        let message = claim.encode();
        let signature =
            TransactionSigner::sign_personal_message(keypair.secret_key(), &message).unwrap();
        let mut proof = BindingProof::new(
            "did:key:z6MkHonest".to_string(),
            address,
            chain_id,
            signature.to_hex(),
            String::from_utf8_lossy(&message).into_owned(),
        );

        // The honest proof binds its metadata.
        assert!(message_binds_metadata(&proof));

        // Editing the DID must now fail *both* checks: the metadata check
        // (the DID is inside the signed claim) and the core `verify()`,
        // which re-binds the proof's public fields to the signed message.
        proof.did = "did:key:z6MkAttacker".to_string();
        assert!(
            !proof.verify().unwrap(),
            "a swapped DID must fail signature/metadata verification"
        );
        assert!(
            !message_binds_metadata(&proof),
            "a swapped DID must not be reported as bound"
        );
    }

    #[test]
    fn bind_create_then_verify_round_trips_and_rejects_tampering() {
        let path = tmp_path("binding.json");
        let mut out = Output::new(OutputKind::Human, true);
        let mut passphrase = Box::new(no_passphrase) as PassphraseResolver;

        let create = BlockchainAction::BindCreate(BindCreateOptions {
            did: "did:key:z6MkSubject".into(),
            chain: "sepolia".into(),
            from_seed: TEST_SEED_HEX.into(),
            expiry_days: Some(30),
            output: Some(path.clone()),
        });
        execute(&create, &mut passphrase, &mut out).expect("bind create must succeed");
        let text = std::fs::read_to_string(&path).unwrap();

        // The written artifact round-trips through the verifier.
        let verify = BlockchainAction::BindVerify(path.clone());
        let mut out = Output::new(OutputKind::Human, true);
        execute(&verify, &mut passphrase, &mut out).expect("bind verify must succeed");

        // The verifier decides validity from the *content*, so drive that
        // content through the same checks it uses rather than scraping the
        // rendered output.
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        let proof: BindingProof = serde_json::from_value(value["proof"].clone()).unwrap();
        assert!(proof.verify().unwrap(), "honest proof must verify");
        assert!(message_binds_metadata(&proof));
        let binding: IdentityBinding = serde_json::from_value(value["binding"].clone()).unwrap();
        assert_eq!(binding.did, proof.did);
        assert_eq!(binding.address.to_hex(), proof.address.to_hex());
        assert_eq!(binding.chain_id, proof.chain_id);

        // Tampering the DID in the artifact must flip the verdict.
        let mut tampered: serde_json::Value = serde_json::from_str(&text).unwrap();
        tampered["proof"]["did"] = serde_json::json!("did:key:z6MkAttacker");
        let tampered_path = tmp_path("binding-tampered.json");
        std::fs::write(&tampered_path, tampered.to_string()).unwrap();
        let tampered_proof: BindingProof =
            serde_json::from_value(tampered["proof"].clone()).unwrap();
        assert!(
            !message_binds_metadata(&tampered_proof),
            "the DID swap must be caught by the metadata check"
        );
        // ... and the command must report failure (exit 2), not render ✗
        // while exiting 0.
        let mut out = Output::new(OutputKind::Human, true);
        assert!(matches!(
            execute(
                &BlockchainAction::BindVerify(tampered_path.clone()),
                &mut passphrase,
                &mut out
            ),
            Err(Web3CliError::VerificationFailed { .. })
        ));

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&tampered_path);
    }

    #[test]
    fn vc_issue_then_verify_round_trips() {
        let path = tmp_path("vc.json");
        let mut passphrase = Box::new(no_passphrase) as PassphraseResolver;

        let issue = BlockchainAction::VcIssue(VcIssueOptions {
            subject_did: "did:key:z6MkSubject".into(),
            credential_type: "RoleCredential".into(),
            claims: vec![("role".into(), serde_json::json!("admin"))],
            issuer: KeySource::Seed(TEST_SEED_HEX.into()),
            valid_until: None,
            output: Some(path.clone()),
        });
        let mut out = Output::new(OutputKind::Human, true);
        execute(&issue, &mut passphrase, &mut out).expect("vc issue must succeed");

        let text = std::fs::read_to_string(&path).unwrap();
        let credential: VerifiableCredential = serde_json::from_str(&text).unwrap();
        assert!(credential.proof.is_some(), "the credential must be signed");
        assert!(
            credential.validate().is_ok(),
            "issued credential must validate"
        );
        assert!(!credential.is_expired());
        assert_eq!(credential.credential_subject.id, "did:key:z6MkSubject");
        assert!(credential
            .credential_type
            .iter()
            .any(|t| t == "RoleCredential"));

        // `vc verify` accepts the same file.
        let verify = BlockchainAction::VcVerify(path.clone());
        let mut out = Output::new(OutputKind::Human, true);
        execute(&verify, &mut passphrase, &mut out).expect("vc verify must succeed");

        // Tampering with a claim must invalidate the Ed25519 proof.
        let mut tampered: serde_json::Value = serde_json::from_str(&text).unwrap();
        tampered["credentialSubject"]["role"] = serde_json::json!("root");
        let tampered_path = tmp_path("vc-tampered.json");
        std::fs::write(&tampered_path, tampered.to_string()).unwrap();
        let mut out = Output::new(OutputKind::Human, true);
        assert!(matches!(
            execute(
                &BlockchainAction::VcVerify(tampered_path.clone()),
                &mut passphrase,
                &mut out
            ),
            Err(Web3CliError::VerificationFailed { .. })
        ));

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&tampered_path);
    }

    #[test]
    fn vc_verify_reports_an_expired_credential_as_invalid() {
        let path = tmp_path("vc-expired.json");
        let mut passphrase = Box::new(no_passphrase) as PassphraseResolver;

        let issue = BlockchainAction::VcIssue(VcIssueOptions {
            subject_did: "did:key:z6MkSubject".into(),
            credential_type: "RoleCredential".into(),
            claims: vec![],
            issuer: KeySource::Seed(TEST_SEED_HEX.into()),
            // Well in the past: `is_expired` compares against now.
            valid_until: Some("2001-01-01T00:00:00Z".into()),
            output: Some(path.clone()),
        });
        let mut out = Output::new(OutputKind::Human, true);
        execute(&issue, &mut passphrase, &mut out).expect("vc issue must succeed");

        let text = std::fs::read_to_string(&path).unwrap();
        let credential: VerifiableCredential = serde_json::from_str(&text).unwrap();
        assert!(
            credential.is_expired(),
            "an expiry in the past must read as expired"
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn chain_list_covers_every_advertised_chain() {
        // The list the executor renders is `KnownChain::ALL`; `Custom` is a
        // placeholder with no configuration and must not appear.
        assert_eq!(KnownChain::ALL.len(), 8);
        assert!(!KnownChain::ALL.contains(&KnownChain::Custom));
        for chain in KnownChain::ALL {
            assert!(
                KnownChain::from_name(chain.name()).is_ok(),
                "{} must resolve by its canonical name",
                chain.name()
            );
        }
    }

    #[test]
    fn tx_build_without_a_network_encodes_the_request() {
        // `tx build` is offline: it encodes an unsigned request. The chain
        // only supplies the chain id, so this runs without a node.
        let mut out = Output::new(OutputKind::Human, true);
        let action = BlockchainAction::TxBuild(TxBuildOptions {
            to: "0x0000000000000000000000000000000000000002".into(),
            value: Some("1.5".into()),
            data: Some("0xdeadbeef".into()),
            chain: "sepolia".into(),
        });
        let mut passphrase = Box::new(no_passphrase) as PassphraseResolver;
        execute(&action, &mut passphrase, &mut out).expect("tx build must succeed offline");
    }

    #[test]
    fn unknown_chain_is_a_core_error_not_a_panic() {
        let mut out = Output::new(OutputKind::Human, true);
        let action = BlockchainAction::ChainInfo("solana".into());
        let mut passphrase = Box::new(no_passphrase) as PassphraseResolver;
        let err = execute(&action, &mut passphrase, &mut out).unwrap_err();
        assert!(
            matches!(err, Web3CliError::Core(_)),
            "unknown chain must surface as a core error, got {err:?}"
        );
    }

    #[test]
    fn emit_echoes_to_stdout_when_no_output_file_is_given() {
        // Regression guard for the no-`--output` path of `bind create`,
        // `vc issue` and `vc present`. It used `write_json_str`, which is a
        // no-op in human mode: `magent web3 vc issue … > cred.json` produced
        // an empty file while the doc promised stdout, and under `--json` it
        // emitted a second object after the caller's own `render_*`. `emit`
        // now reports which path it took so the caller can suppress its
        // rendering — and reaching stdout in human mode is the whole point.
        let mut human = Output::new(OutputKind::Human, true);
        assert!(
            emit("{\"a\":1}", None, &mut human, "credential").unwrap(),
            "no --output must echo the artifact to stdout"
        );

        let path = tmp_path("emit-echo.json");
        let mut human2 = Output::new(OutputKind::Human, true);
        assert!(
            !emit("{\"a\":1}", Some(&path), &mut human2, "credential").unwrap(),
            "--output must write the file, not stdout"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\"a\":1}");
        let _ = std::fs::remove_file(&path);
    }
}
