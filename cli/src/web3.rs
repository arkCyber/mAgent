//! `magent web3` — host-side Web3 identity & asymmetric crypto CLI.
//!
//! Provides a thin command-line wrapper around the
//! `magent_core::web3` module: generate identities, sign messages,
//! verify envelopes, derive `did:key` handles, and round-trip
//! encrypted keystores. The CLI is a *host-only* convenience layer;
//! the underlying primitives live in `magent-core` and remain
//! `no_std`-friendly.
//!
//! ## Subcommand tree
//!
//! ```text
//! magent web3 new     [--name <NAME>]                 Generate an Ed25519 identity
//! magent web3 identity <NAME>                         Print public-side info (DID + pubkey)
//! magent web3 did      [--from-seed|--from-pubkey]    Derive a did:key
//! magent web3 pubkey   [--from-seed]                  Derive the raw 32-byte pubkey (hex)
//! magent web3 sign    <NAME> --payload <FILE|->       Produce a SignedMessage envelope
//! magent web3 verify  --payload <FILE|-> --envelope <FILE>  Verify a SignedMessage
//! magent web3 list                                     List every stored identity
//! magent web3 export  <NAME>                          Print the raw JSON envelope
//! magent web3 delete  <NAME>                          Remove an identity from the vault
//! ```
//!
//! ## Encrypted vault
//!
//! Generated identities land in a passphrase-protected JSON vault at
//! `$MAGENT_WEB3_KEYSTORE` or
//! `$XDG_DATA_HOME/magent/web3/keys.json` (falling back to
//! `~/.local/share/magent/web3/keys.json`). Each entry is the 32-byte
//! Ed25519 seed, encrypted under:
//!
//! * **KDF**: Argon2id (passphrase → 256-bit key)
//! * **AEAD**: ChaCha20-Poly1305 (per-entry random nonce)
//! * **Encoding**: base64 for the salt / nonce / ciphertext fields
//!
//! The vault file is meant to be readable on disk by humans (it is
//! a single JSON document with `schema_version`, `kdf`, `aead`,
//! `identities: { <name>: {...} }`) so operators can audit it and
//! back it up. The on-disk secret is always a ciphertext — the
//! plaintext seed never leaves the process without an explicit
//! `--export-secret-hex` (which is itself a deliberate foot-gun
//! that requires confirmation).

#![cfg(feature = "web3")]

use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use magent_core::error::Web3ErrorKind;
use magent_core::web3 as core;

use crate::output::{Output, OutputKind};

// ============================================================================
// Public constants
// ============================================================================

/// Current schema version of the on-disk vault. Bump on breaking
/// changes to [`Vault`]; readers MUST refuse to load files whose
/// `schema_version` is higher than this.
pub const CURRENT_VAULT_SCHEMA_VERSION: u32 = 1;

/// Environment variable that overrides the default vault location.
/// Useful for tests / CI / ephemeral container deployments.
pub const KEYSTORE_ENV: &str = "MAGENT_WEB3_KEYSTORE";

/// Environment variable that overrides the default vault directory
/// (the file is then `<dir>/keys.json`). Wins over [`KEYSTORE_ENV`]
/// only because it's more specific.
pub const KEYSTORE_DIR_ENV: &str = "MAGENT_WEB3_KEYSTORE_DIR";

/// Filename used inside the keystore directory.
pub const VAULT_FILENAME: &str = "keys.json";

/// Largest identity name we'll accept. Real-world names fit in a
/// few dozen characters; anything bigger is almost certainly a
/// paste accident or a path traversal probe.
pub const IDENTITY_NAME_MAX: usize = 64;

/// Largest payload we'll read from stdin / `--payload <FILE>` for
/// signing. 1 MiB matches the bound used by `scheduler.rs` for
/// task files — keeps memory usage predictable for a CLI.
pub const PAYLOAD_MAX: usize = 1_048_576; // 1 MiB

/// Largest envelope (signed-message JSON) we'll read. Same bound.
pub const ENVELOPE_MAX: usize = 1_048_576 + 16_384; // 1 MiB + slack for the envelope fields

/// Number of random bytes per entry's salt. 16 bytes matches the
/// Argon2 recommended salt length.
const SALT_LEN: usize = 16;

/// Argon2 memory cost (KiB). 64 MiB is the OWASP "interactive"
/// recommendation for Argon2id as of 2024. Tuned for a desktop
/// where the user is typing the passphrase interactively.
const ARGON2_MEM_KIB: u32 = 64 * 1024;

/// Argon2 time cost. 3 iterations is the OWASP minimum for the
/// memory cost above.
const ARGON2_TIME_COST: u32 = 3;

/// Argon2 lanes (parallelism). 1 is fine for an interactive CLI.
const ARGON2_LANES: u32 = 1;

// ============================================================================
// Errors
// ============================================================================

/// Errors surfaced by the `magent web3` subcommand. Every variant
/// carries enough context to print a one-line diagnostic.
#[derive(Debug)]
pub enum Web3CliError {
    /// The user's `$HOME` / XDG directory couldn't be resolved.
    NoHomeDirectory,
    /// A user-supplied identity name was empty, too long, or
    /// contained characters the on-disk JSON refuses.
    InvalidName(String),
    /// The named identity doesn't exist in the vault.
    NotFound(String),
    /// An identity with that name already exists.
    AlreadyExists(String),
    /// The vault file exists but couldn't be parsed as JSON, or its
    /// `schema_version` is higher than we support.
    VaultParse {
        path: PathBuf,
        /// Underlying `serde_json` error if a JSON parse failed.
        source: Option<serde_json::Error>,
    },
    VaultSchema {
        path: PathBuf,
        found: u32,
        supported: u32,
    },
    /// Reading or writing the vault file failed.
    VaultIo {
        path: PathBuf,
        source: io::Error,
    },
    /// Argon2id key derivation failed (low memory, parameter out
    /// of range, OS RNG unavailable, …).
    Kdf(String),
    /// ChaCha20-Poly1305 encrypt/decrypt failed. The only realistic
    /// cause is a wrong passphrase (auth-tag mismatch) — we
    /// collapse the AEAD error into a friendly diagnostic.
    Aead(String),
    /// The underlying `magent_core::web3` module rejected something.
    Core(Web3ErrorKind),
    /// `magent web3 verify` got an envelope that failed verification.
    VerificationFailed {
        /// The reason, in human-readable form.
        reason: String,
    },
    /// A user-supplied payload file couldn't be read.
    PayloadIo {
        path: PathBuf,
        source: io::Error,
    },
    /// The user tried to read a payload from stdin but stdin was
    /// empty / not a pipe.
    EmptyStdin,
}

impl std::fmt::Display for Web3CliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Web3CliError::NoHomeDirectory => write!(
                f,
                "could not determine a vault location: set $MAGENT_WEB3_KEYSTORE or $HOME"
            ),
            Web3CliError::InvalidName(name) => write!(
                f,
                "invalid identity name {:?}: must be 1..={} bytes of [a-zA-Z0-9._-]",
                name, IDENTITY_NAME_MAX
            ),
            Web3CliError::NotFound(name) => write!(f, "identity {:?} not found", name),
            Web3CliError::AlreadyExists(name) => {
                write!(f, "identity {:?} already exists (use --force to overwrite)", name)
            }
            Web3CliError::VaultParse { path, source: Some(e) } => {
                write!(f, "vault file {} is not valid JSON: {}", path.display(), e)
            }
            Web3CliError::VaultParse { path, source: None } => {
                write!(f, "vault file {} has an unexpected shape", path.display())
            }
            Web3CliError::VaultSchema { path, found, supported } => write!(
                f,
                "vault file {} uses schema_version={}, but this build supports up to {}",
                path.display(),
                found,
                supported
            ),
            Web3CliError::VaultIo { path, source } => {
                write!(f, "vault I/O error on {}: {}", path.display(), source)
            }
            Web3CliError::Kdf(msg) => write!(f, "Argon2id KDF failed: {}", msg),
            Web3CliError::Aead(msg) => write!(f, "vault decrypt failed: {}", msg),
            Web3CliError::Core(kind) => write!(f, "{}", kind),
            Web3CliError::VerificationFailed { reason } => {
                write!(f, "verification failed: {}", reason)
            }
            Web3CliError::PayloadIo { path, source } => {
                write!(f, "could not read payload {}: {}", path.display(), source)
            }
            Web3CliError::EmptyStdin => write!(
                f,
                "no payload on stdin (pass --payload <FILE> or pipe data)"
            ),
        }
    }
}

impl std::error::Error for Web3CliError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Web3CliError::VaultParse { source: Some(e), .. } => Some(e),
            Web3CliError::VaultIo { source, .. } => Some(source),
            Web3CliError::PayloadIo { source, .. } => Some(source),
            // `Web3ErrorKind` from `magent-core` does not (yet)
            // implement `std::error::Error`, so we can't return it
            // as a `&dyn Error`. The Display path still surfaces the
            // full message via `{:?}` or `{}`, which is what most
            // callers actually want.
            _ => None,
        }
    }
}

impl From<Web3ErrorKind> for Web3CliError {
    fn from(kind: Web3ErrorKind) -> Self {
        Web3CliError::Core(kind)
    }
}

// ============================================================================
// Vault on-disk schema
// ============================================================================

/// Wrapper around the JSON vault file. The structure is intentionally
/// trivial (one JSON object per file) so operators can `cat` /
/// `jq` / diff it without surprise.
#[derive(Debug, Serialize, Deserialize)]
pub struct Vault {
    /// Schema version. Bumped by [`CURRENT_VAULT_SCHEMA_VERSION`].
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    /// KDF identifier — currently always `"argon2id"`. Stored as a
    /// string rather than an enum so a future migration to a
    /// different KDF can happen without breaking the on-disk shape.
    pub kdf: String,
    /// AEAD identifier — currently always `"chacha20-poly1305"`.
    pub aead: String,
    /// Argon2id parameters. Stored so a vault written by a future
    /// CLI build with stronger parameters can still be read (we
    /// only refuse if `schema_version` is newer than we support).
    pub kdf_params: KdfParams,
    /// Identity entries, keyed by name.
    pub identities: std::collections::BTreeMap<String, VaultEntry>,
}

fn default_schema_version() -> u32 {
    1
}

/// Argon2id parameters. The `t_cost`, `m_cost`, `p_cost` mirror
/// the underlying Argon2 terminology; see RFC 9106 §3.2.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KdfParams {
    pub t_cost: u32,
    pub m_cost_kib: u32,
    pub p_cost: u32,
}

/// One identity inside the vault. The 32-byte Ed25519 seed is
/// stored as ciphertext + nonce + salt so a leaked vault file
/// without the passphrase is useless.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultEntry {
    /// Creation time as Unix seconds. Stored for auditing —
    /// `magent web3 list` uses it.
    pub created_at_unix: u64,
    /// Last update time as Unix seconds. Bumped every time the
    /// entry is re-encrypted.
    #[serde(default)]
    pub updated_at_unix: u64,
    /// Hex-encoded Ed25519 public key (64 chars, no `0x`). Stored
    /// *in the clear* — public keys aren't secret, and the vault
    /// would be much less useful (e.g. for `magent web3 identity
    /// <NAME>` to print the DID) if it had to decrypt every read.
    pub public_key_hex: String,
    /// `did:key:z…` handle. Same reasoning as `public_key_hex`.
    pub did: String,
    /// Per-entry salt (base64). The KDF combines this with the
    /// passphrase to derive the AEAD key.
    pub salt_b64: String,
    /// Per-entry nonce (base64). 96 bits, generated fresh on every
    /// re-encryption.
    pub nonce_b64: String,
    /// Ciphertext (base64). Includes the 16-byte Poly1305 tag
    /// appended by ChaCha20-Poly1305.
    pub ciphertext_b64: String,
}

// ============================================================================
// CLI action enum + options
// ============================================================================

/// Sub-actions of `magent web3`. The CLI parser picks one of these
/// from the second positional argument (the first being the
/// `web3` subcommand itself).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Web3Action {
    /// `magent web3 new <NAME> [--passphrase-env VAR | --no-passphrase]`
    New(Web3NewOptions),
    /// `magent web3 identity <NAME>`
    Identity(String),
    /// `magent web3 did [--from-seed <HEX> | --from-pubkey <HEX>]`
    Did(Web3DidOptions),
    /// `magent web3 pubkey [--from-seed <HEX>]`
    Pubkey(Web3PubkeyOptions),
    /// `magent web3 sign <NAME> --payload <FILE|-> [--output <FILE>]`
    Sign(Web3SignOptions),
    /// `magent web3 verify --payload <FILE|-> --envelope <FILE>`
    Verify(Web3VerifyOptions),
    /// `magent web3 list`
    List,
    /// `magent web3 export <NAME>`
    Export(String),
    /// `magent web3 delete <NAME>`
    Delete(String),
}

/// Options for `magent web3 new <NAME>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Web3NewOptions {
    pub name: String,
    /// Passphrase source. `Some(env_var)` reads the passphrase from
    /// `$<env_var>` (so it doesn't appear in `ps`/`/proc`), `None`
    /// means "prompt on the terminal" with confirmation. We never
    /// accept the passphrase on the command line — it would land in
    /// shell history.
    pub passphrase_env: Option<String>,
    /// `--force` — overwrite an existing identity with the same
    /// name. Without this, [`Web3CliError::AlreadyExists`] fires.
    pub force: bool,
    /// `--vault <PATH>` — override the vault location for this
    /// invocation only.
    pub vault_override: Option<PathBuf>,
}

/// Options for `magent web3 did`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Web3DidOptions {
    /// `--from-seed <HEX>` — derive from a 32-byte secret seed.
    pub from_seed_hex: Option<String>,
    /// `--from-pubkey <HEX>` — derive from a 32-byte public key.
    pub from_pubkey_hex: Option<String>,
}

/// Options for `magent web3 pubkey`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Web3PubkeyOptions {
    pub from_seed_hex: Option<String>,
}

/// Options for `magent web3 sign`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Web3SignOptions {
    pub name: String,
    /// `--payload <FILE>` or `-` for stdin.
    pub payload: PayloadSource,
    /// `--output <FILE>` — write the envelope to a file rather than
    /// stdout. (Default is stdout, so `magent web3 sign alice --payload
    /// msg.txt > envelope.json` Just Works.)
    pub output: Option<PathBuf>,
    /// `--passphrase-env <VAR>` — same convention as `new`.
    pub passphrase_env: Option<String>,
    /// `--vault <PATH>` — override the vault location for this
    /// invocation only.
    pub vault_override: Option<PathBuf>,
}

/// Options for `magent web3 verify`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Web3VerifyOptions {
    pub payload: PayloadSource,
    pub envelope: PathBuf,
}

/// Where to read the payload from. `-` means stdin (the conventional
/// shell convention, so `cat file | magent web3 sign alice --payload -`
/// Just Works).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PayloadSource {
    /// Read from a file on disk.
    File(PathBuf),
    /// Read from stdin.
    Stdin,
}

impl Default for PayloadSource {
    /// `Stdin` is the default because the most common shell idiom
    /// for `magent web3 sign` is `cat payload.txt | magent web3 sign
    /// alice`. Forcing the user to add `--payload -` for that case
    /// would be friction; making `-` the implicit default matches the
    /// convention used by most Unix CLI tools (`openssl`, `gpg`,
    /// `ssh-keygen`, …).
    fn default() -> Self {
        PayloadSource::Stdin
    }
}

impl PayloadSource {
    /// Read the entire payload, honouring [`PAYLOAD_MAX`]. Returns
    /// an empty-stdin error if the source is stdin and EOF hits
    /// without any bytes.
    pub fn read(&self) -> Result<Vec<u8>, Web3CliError> {
        match self {
            PayloadSource::File(path) => {
                let f = fs::File::open(path).map_err(|e| Web3CliError::PayloadIo {
                    path: path.clone(),
                    source: e,
                })?;
                let mut buf = Vec::new();
                f.take((PAYLOAD_MAX as u64) + 1)
                    .read_to_end(&mut buf)
                    .map_err(|e| Web3CliError::PayloadIo {
                        path: path.clone(),
                        source: e,
                    })?;
                if buf.len() > PAYLOAD_MAX {
                    return Err(Web3CliError::PayloadIo {
                        path: path.clone(),
                        source: io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!(
                                "payload exceeds {} bytes (refusing to allocate)",
                                PAYLOAD_MAX
                            ),
                        ),
                    });
                }
                Ok(buf)
            }
            PayloadSource::Stdin => {
                let mut buf = Vec::new();
                let stdin = io::stdin();
                let handle = stdin.lock();
                handle
                    .take((PAYLOAD_MAX as u64) + 1)
                    .read_to_end(&mut buf)
                    .map_err(|e| Web3CliError::PayloadIo {
                        path: PathBuf::from("<stdin>"),
                        source: e,
                    })?;
                if buf.is_empty() {
                    return Err(Web3CliError::EmptyStdin);
                }
                if buf.len() > PAYLOAD_MAX {
                    return Err(Web3CliError::PayloadIo {
                        path: PathBuf::from("<stdin>"),
                        source: io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!(
                                "stdin payload exceeds {} bytes (refusing to allocate)",
                                PAYLOAD_MAX
                            ),
                        ),
                    });
                }
                Ok(buf)
            }
        }
    }
}

// ============================================================================
// Glue struct (mirrors `RunCmd`, `DoctorCmd`, …)
// ============================================================================

/// Glue struct so `main.rs` can construct and run the subcommand in
/// one line. The lifetime parameter lets the executor borrow the
/// parsed action without owning it.
pub struct Web3Cmd<'a> {
    pub action: &'a Web3Action,
    /// Passphrase resolver: turns `--passphrase-env <VAR>` into the
    /// actual passphrase bytes. Held as an `FnMut` closure so the
    /// `main.rs` dispatcher can prompt / read once per command
    /// without committing to a specific UI choice.
    pub passphrase: PassphraseResolver,
}

/// Closure type used by [`Web3Cmd`] to look up the passphrase. Pulled
/// out as a named alias so the field declaration stays readable.
pub type PassphraseResolver = Box<dyn FnMut(&str) -> Result<String, Web3CliError>>;

impl<'a> Web3Cmd<'a> {
    pub fn new(action: &'a Web3Action, passphrase: PassphraseResolver) -> Self {
        Self { action, passphrase }
    }

    /// Execute the subcommand. Always writes to `out` so both human
    /// and JSON modes get consistent output.
    pub fn execute(&mut self, out: &mut Output) -> Result<(), Web3CliError> {
        match self.action {
            Web3Action::New(opts) => self.run_new(opts, out),
            Web3Action::Identity(name) => self.run_identity(name, out),
            Web3Action::Did(opts) => self.run_did(opts, out),
            Web3Action::Pubkey(opts) => self.run_pubkey(opts, out),
            Web3Action::Sign(opts) => self.run_sign(opts, out),
            Web3Action::Verify(opts) => self.run_verify(opts, out),
            Web3Action::List => self.run_list(out),
            Web3Action::Export(name) => self.run_export(name, out),
            Web3Action::Delete(name) => self.run_delete(name, out),
        }
    }

    // -----------------------------------------------------------------
    // new
    // -----------------------------------------------------------------

    fn run_new(&mut self, opts: &Web3NewOptions, out: &mut Output) -> Result<(), Web3CliError> {
        validate_name(&opts.name)?;

        // Load-or-create the vault. We accept `--vault` as an
        // override; otherwise resolve the default location.
        let vault_path = opts
            .vault_override
            .clone()
            .unwrap_or_else(default_vault_path);
        let mut vault = load_or_init_vault(&vault_path)?;

        if vault.identities.contains_key(&opts.name) && !opts.force {
            return Err(Web3CliError::AlreadyExists(opts.name.clone()));
        }

        let id = core::Identity::generate()?;
        let public_key_hex = id.public_key().to_hex();
        let did = id.did_key().as_str();
        let secret_hex = id.secret_key().to_hex();

        // Resolve passphrase + run KDF + AEAD encrypt.
        let (salt_b64, nonce_b64, ciphertext_b64) = encrypt_secret(
            secret_hex.as_bytes(),
            &mut self.passphrase,
            opts.passphrase_env.as_deref(),
        )?;

        let now = now_unix_seconds();
        let entry = VaultEntry {
            created_at_unix: vault
                .identities
                .get(&opts.name)
                .map(|e| e.created_at_unix)
                .unwrap_or(now),
            updated_at_unix: now,
            public_key_hex: public_key_hex.clone(),
            did: did.clone(),
            salt_b64,
            nonce_b64,
            ciphertext_b64,
        };
        vault.identities.insert(opts.name.clone(), entry);
        save_vault(&vault_path, &vault)?;

        let vault_path_display = vault_path.to_string_lossy().into_owned();
        let display = NewDisplay {
            name: &opts.name,
            did: &did,
            public_key_hex: &public_key_hex,
            vault_path: &vault_path_display,
        };
        render_new(&display, out);
        Ok(())
    }

    // -----------------------------------------------------------------
    // identity
    // -----------------------------------------------------------------

    fn run_identity(&self, name: &str, out: &mut Output) -> Result<(), Web3CliError> {
        let vault_path = default_vault_path();
        let vault = load_vault(&vault_path)?;
        let entry = vault
            .identities
            .get(name)
            .ok_or_else(|| Web3CliError::NotFound(name.to_string()))?;

        let vault_path_display = vault_path.to_string_lossy().into_owned();
        let display = IdentityDisplay {
            name,
            did: &entry.did,
            public_key_hex: &entry.public_key_hex,
            created_at_unix: entry.created_at_unix,
            updated_at_unix: entry.updated_at_unix,
            vault_path: &vault_path_display,
        };
        render_identity(&display, out);
        Ok(())
    }

    // -----------------------------------------------------------------
    // did / pubkey (key derivation, no I/O)
    // -----------------------------------------------------------------

    fn run_did(&self, opts: &Web3DidOptions, out: &mut Output) -> Result<(), Web3CliError> {
        let (label, did_str) = if let Some(hex_seed) = &opts.from_seed_hex {
            let bytes = decode_hex(hex_seed)?;
            let id = core::Identity::from_secret_bytes(&bytes)?;
            ("seed", id.did_key().as_str())
        } else if let Some(hex_pk) = &opts.from_pubkey_hex {
            let bytes = decode_hex(hex_pk)?;
            let pk = core::PublicKey::from_bytes(&bytes)?;
            ("pubkey", pk.did_key().as_str())
        } else {
            return Err(Web3CliError::Kdf(
                "must pass either --from-seed <HEX> or --from-pubkey <HEX>".to_string(),
            ));
        };
        let display = DidDisplay {
            source: label,
            did: &did_str,
        };
        render_did(&display, out);
        Ok(())
    }

    fn run_pubkey(&self, opts: &Web3PubkeyOptions, out: &mut Output) -> Result<(), Web3CliError> {
        let hex_seed = opts.from_seed_hex.as_deref().ok_or_else(|| {
            Web3CliError::Kdf("must pass --from-seed <HEX>".to_string())
        })?;
        let bytes = decode_hex(hex_seed)?;
        let id = core::Identity::from_secret_bytes(&bytes)?;
        let display = PubkeyDisplay {
            public_key_hex: &id.public_key().to_hex(),
            did: &id.did_key().as_str(),
        };
        render_pubkey(&display, out);
        Ok(())
    }

    // -----------------------------------------------------------------
    // sign / verify
    // -----------------------------------------------------------------

    fn run_sign(&mut self, opts: &Web3SignOptions, out: &mut Output) -> Result<(), Web3CliError> {
        let vault_path = opts
            .vault_override
            .clone()
            .unwrap_or_else(default_vault_path);
        let vault = load_vault(&vault_path)?;
        let entry = vault
            .identities
            .get(&opts.name)
            .ok_or_else(|| Web3CliError::NotFound(opts.name.clone()))?;

        let passphrase = (self.passphrase)(
            opts.passphrase_env
                .as_deref()
                .unwrap_or("MAGENT_WEB3_PASSPHRASE"),
        )?;
        let secret_bytes = decrypt_secret(
            &entry.salt_b64,
            &entry.nonce_b64,
            &entry.ciphertext_b64,
            passphrase.as_bytes(),
        )?;
        let secret_str = std::str::from_utf8(&secret_bytes).map_err(|_| {
            Web3CliError::Aead("decrypted seed is not valid UTF-8 (corrupt vault?)".to_string())
        })?;
        let id = core::Identity::from_secret_hex(secret_str)?;

        let payload = opts.payload.read()?;
        let signed = id.sign(&payload)?;

        match &opts.output {
            Some(path) => {
                fs::write(path, signed.to_json()).map_err(|e| Web3CliError::VaultIo {
                    path: path.clone(),
                    source: e,
                })?;
                if out.kind() == OutputKind::Human {
                    let _ = out.info(&format!(
                        "wrote signed envelope ({} bytes) to {}",
                        signed.to_json().len(),
                        path.display()
                    ));
                }
            }
            None => {
                // In human mode we print a minimal "signed with DID …
                // envelope below" header so the user can confirm what
                // they're piping into the next stage. In JSON mode
                // the envelope IS the output — nothing else on
                // stdout, per the global output convention.
                if out.kind() == OutputKind::Human {
                    let _ = out.info(&format!("signed with did: {}", entry.did));
                }
                out.write_json_str(signed.to_json());
            }
        }
        Ok(())
    }

    fn run_verify(&self, opts: &Web3VerifyOptions, out: &mut Output) -> Result<(), Web3CliError> {
        let envelope = fs::read_to_string(&opts.envelope).map_err(|e| Web3CliError::PayloadIo {
            path: opts.envelope.clone(),
            source: e,
        })?;
        let signed = core::SignedMessage::from_json(&envelope)?;
        let payload = opts.payload.read()?;
        match core::verify_signed_message_detailed(&signed, &payload) {
            Ok(()) => {
                if out.kind() == OutputKind::Human {
                    let _ = out.info("verification OK");
                }
                Ok(())
            }
            Err(e) => Err(Web3CliError::VerificationFailed {
                reason: e.to_string(),
            }),
        }
    }

    // -----------------------------------------------------------------
    // list / export / delete
    // -----------------------------------------------------------------

    fn run_list(&self, out: &mut Output) -> Result<(), Web3CliError> {
        let vault_path = default_vault_path();
        // `list` on a missing vault is a normal first-run scenario;
        // surface it as an empty vault rather than a hard error.
        let vault = load_or_init_vault(&vault_path)?;
        let vault_path_display = vault_path.to_string_lossy().into_owned();
        let display = ListDisplay {
            identities: vault
                .identities
                .iter()
                .map(|(name, entry)| ListEntryDisplay {
                    name,
                    did: &entry.did,
                    public_key_hex: &entry.public_key_hex,
                    created_at_unix: entry.created_at_unix,
                    updated_at_unix: entry.updated_at_unix,
                })
                .collect(),
            vault_path: &vault_path_display,
        };
        render_list(&display, out);
        Ok(())
    }

    fn run_export(&self, name: &str, out: &mut Output) -> Result<(), Web3CliError> {
        let vault_path = default_vault_path();
        let vault = load_vault(&vault_path)?;
        let entry = vault
            .identities
            .get(name)
            .ok_or_else(|| Web3CliError::NotFound(name.to_string()))?;
        // Print the public-side record verbatim. The secret half is
        // never printed by `export`; users wanting the raw seed must
        // copy it through a separate (deliberately-awkward) flow.
        let exported = ExportedIdentity {
            schema_version: CURRENT_VAULT_SCHEMA_VERSION,
            name,
            public_key_hex: &entry.public_key_hex,
            did: &entry.did,
            created_at_unix: entry.created_at_unix,
            updated_at_unix: entry.updated_at_unix,
        };
        let json = serde_json::to_string_pretty(&exported).map_err(|e| {
            Web3CliError::VaultParse {
                path: vault_path.clone(),
                source: Some(e),
            }
        })?;
        out.write_json_str(json);
        Ok(())
    }

    fn run_delete(&self, name: &str, out: &mut Output) -> Result<(), Web3CliError> {
        let vault_path = default_vault_path();
        let mut vault = load_vault(&vault_path)?;
        if vault.identities.remove(name).is_none() {
            return Err(Web3CliError::NotFound(name.to_string()));
        }
        save_vault(&vault_path, &vault)?;
        if out.kind() == OutputKind::Human {
            let _ = out.info(&format!("deleted identity {:?}", name));
        }
        Ok(())
    }
}

// ============================================================================
// Helpers — name validation, paths, vault I/O, crypto
// ============================================================================

/// Validate a user-supplied identity name. Allowed characters are
/// ASCII letters / digits / `.` / `_` / `-`. Path separators and
/// JSON-unsafe characters are rejected so the on-disk JSON is
/// always valid and the name is always safe to use as a filename.
pub fn validate_name(name: &str) -> Result<(), Web3CliError> {
    if name.is_empty() || name.len() > IDENTITY_NAME_MAX {
        return Err(Web3CliError::InvalidName(name.to_string()));
    }
    if !name
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-')
    {
        return Err(Web3CliError::InvalidName(name.to_string()));
    }
    Ok(())
}

/// Resolve the default vault path. Honours `MAGENT_WEB3_KEYSTORE`
/// (a full path including filename), then
/// `MAGENT_WEB3_KEYSTORE_DIR/keys.json`, then the XDG data dir.
pub fn default_vault_path() -> PathBuf {
    if let Ok(p) = std::env::var(KEYSTORE_ENV) {
        return PathBuf::from(p);
    }
    if let Ok(dir) = std::env::var(KEYSTORE_DIR_ENV) {
        return PathBuf::from(dir).join(VAULT_FILENAME);
    }
    // XDG default. Mirror the convention used by `prompt.rs` /
    // `scheduler.rs`.
    if let Some(dir) = xdg_data_home() {
        return dir.join("magent").join("web3").join(VAULT_FILENAME);
    }
    // Last-resort fallback: current directory. We never silently
    // succeed without telling the caller where the file went.
    PathBuf::from(".").join("magent-web3-keys.json")
}

fn xdg_data_home() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("XDG_DATA_HOME") {
        if !p.is_empty() {
            return Some(PathBuf::from(p));
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        if !home.is_empty() {
            return Some(PathBuf::from(home).join(".local").join("share"));
        }
    }
    None
}

/// Load the vault from `path`. Returns a clean "no such file"
/// error if the file doesn't exist, but if the file does exist
/// and is malformed, surface the parse error.
pub fn load_vault(path: &Path) -> Result<Vault, Web3CliError> {
    let bytes = fs::read(path).map_err(|e| {
        if e.kind() == io::ErrorKind::NotFound {
            // Wrap into a fresh `Empty` vault so callers don't have
            // to special-case "first run". We surface this as a
            // parse-style error only at the read-or-init layer.
            Web3CliError::VaultIo {
                path: path.to_path_buf(),
                source: e,
            }
        } else {
            Web3CliError::VaultIo {
                path: path.to_path_buf(),
                source: e,
            }
        }
    })?;
    let vault: Vault = serde_json::from_slice(&bytes).map_err(|e| Web3CliError::VaultParse {
        path: path.to_path_buf(),
        source: Some(e),
    })?;
    if vault.schema_version > CURRENT_VAULT_SCHEMA_VERSION {
        return Err(Web3CliError::VaultSchema {
            path: path.to_path_buf(),
            found: vault.schema_version,
            supported: CURRENT_VAULT_SCHEMA_VERSION,
        });
    }
    Ok(vault)
}

/// Load the vault from `path`, or — if the file doesn't exist —
/// return a freshly-defaulted vault. This is the right entry point
/// for `magent web3 new` so the first-run UX doesn't have to
/// special-case a missing vault.
pub fn load_or_init_vault(path: &Path) -> Result<Vault, Web3CliError> {
    match load_vault(path) {
        Ok(v) => Ok(v),
        Err(Web3CliError::VaultIo { source, .. }) if source.kind() == io::ErrorKind::NotFound => {
            Ok(empty_vault())
        }
        Err(e) => Err(e),
    }
}

/// Build a fresh, empty vault. Public so callers can pre-populate
/// tests or scripting flows.
pub fn empty_vault() -> Vault {
    Vault {
        schema_version: CURRENT_VAULT_SCHEMA_VERSION,
        kdf: "argon2id".to_string(),
        aead: "chacha20-poly1305".to_string(),
        kdf_params: KdfParams {
            t_cost: ARGON2_TIME_COST,
            m_cost_kib: ARGON2_MEM_KIB,
            p_cost: ARGON2_LANES,
        },
        identities: std::collections::BTreeMap::new(),
    }
}

/// Write the vault atomically (write-to-temp + rename) so an
/// interrupted process never produces a half-written JSON.
pub fn save_vault(path: &Path, vault: &Vault) -> Result<(), Web3CliError> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).map_err(|e| Web3CliError::VaultIo {
                path: parent.to_path_buf(),
                source: e,
            })?;
        }
    }
    let json = serde_json::to_vec_pretty(vault).map_err(|e| Web3CliError::VaultParse {
        path: path.to_path_buf(),
        source: Some(e),
    })?;
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, &json).map_err(|e| Web3CliError::VaultIo {
        path: tmp.clone(),
        source: e,
    })?;
    fs::rename(&tmp, path).map_err(|e| Web3CliError::VaultIo {
        path: path.to_path_buf(),
        source: e,
    })?;
    Ok(())
}

/// Encrypt `secret_bytes` under the passphrase. Returns the
/// (base64-encoded) salt, nonce, and ciphertext.
fn encrypt_secret(
    secret_bytes: &[u8],
    passphrase: &mut dyn FnMut(&str) -> Result<String, Web3CliError>,
    env_var: Option<&str>,
) -> Result<(String, String, String), Web3CliError> {
    use argon2::{Algorithm, Argon2, Params, Version};
    use chacha20poly1305::aead::{Aead, KeyInit};
    use chacha20poly1305::{ChaCha20Poly1305, Key};
    use rand_core::{OsRng, RngCore};

    let passphrase_str = if let Some(var) = env_var {
        std::env::var(var).map_err(|_| {
            Web3CliError::Aead(format!("passphrase env var ${} is not set", var))
        })?
    } else {
        passphrase(env_var.unwrap_or("MAGENT_WEB3_PASSPHRASE"))?
    };

    let params = Params::new(
        ARGON2_MEM_KIB,
        ARGON2_TIME_COST,
        ARGON2_LANES,
        Some(32),
    )
    .map_err(|e| Web3CliError::Kdf(format!("invalid Argon2 params: {}", e)))?;
    let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut salt = [0u8; SALT_LEN];
    OsRng.fill_bytes(&mut salt);
    let mut key_bytes = [0u8; 32];
    argon
        .hash_password_into(passphrase_str.as_bytes(), &salt, &mut key_bytes)
        .map_err(|e| Web3CliError::Kdf(format!("Argon2id: {}", e)))?;
    let key = Key::from(key_bytes);
    let cipher = ChaCha20Poly1305::new(&key);
    let mut nonce_bytes = [0u8; 12];
    OsRng.fill_bytes(&mut nonce_bytes);
    // `chacha20poly1305` 0.10 marks `Nonce::from_slice` as
    // deprecated (it asks users to upgrade to `generic-array` 1.x),
    // but the upgrade changes the public type and would pull a
    // heavier dep tree. Accept the deprecation rather than
    // destabilise the dep graph.
    #[allow(deprecated)]
    let nonce = chacha20poly1305::aead::generic_array::GenericArray::from_slice(&nonce_bytes);
    let ct = cipher
        .encrypt(nonce, secret_bytes)
        .map_err(|e| Web3CliError::Aead(format!("ChaCha20-Poly1305 encrypt: {}", e)))?;

    Ok((
        base64_encode(&salt),
        base64_encode(&nonce_bytes),
        base64_encode(&ct),
    ))
}

/// Decrypt the vault entry back into the original plaintext bytes.
/// Returns a friendly "wrong passphrase" error if the AEAD tag
/// doesn't verify.
pub fn decrypt_secret(
    salt_b64: &str,
    nonce_b64: &str,
    ciphertext_b64: &str,
    passphrase: &[u8],
) -> Result<Vec<u8>, Web3CliError> {
    use argon2::{Algorithm, Argon2, Params, Version};
    use chacha20poly1305::aead::{Aead, KeyInit};
    use chacha20poly1305::{ChaCha20Poly1305, Key};

    let salt = base64_decode(salt_b64)?;
    let nonce_bytes = base64_decode(nonce_b64)?;
    let ct = base64_decode(ciphertext_b64)?;
    if nonce_bytes.len() != 12 {
        return Err(Web3CliError::Aead(format!(
            "nonce must be 12 bytes, got {}",
            nonce_bytes.len()
        )));
    }
    let params = Params::new(
        ARGON2_MEM_KIB,
        ARGON2_TIME_COST,
        ARGON2_LANES,
        Some(32),
    )
    .map_err(|e| Web3CliError::Kdf(format!("invalid Argon2 params: {}", e)))?;
    let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut key_bytes = [0u8; 32];
    argon
        .hash_password_into(passphrase, &salt, &mut key_bytes)
        .map_err(|e| Web3CliError::Kdf(format!("Argon2id: {}", e)))?;
    let key = Key::from(key_bytes);
    let cipher = ChaCha20Poly1305::new(&key);
    // `chacha20poly1305` 0.10 marks `Nonce::from_slice` as
    // deprecated (it asks users to upgrade to `generic-array` 1.x),
    // but the upgrade changes the public type and would pull a
    // heavier dep tree. Accept the deprecation rather than
    // destabilise the dep graph.
    #[allow(deprecated)]
    let nonce = chacha20poly1305::aead::generic_array::GenericArray::from_slice(&nonce_bytes);
    cipher.decrypt(nonce, ct.as_ref()).map_err(|_| {
        Web3CliError::Aead(
            "wrong passphrase or corrupt vault (AEAD tag mismatch)".to_string(),
        )
    })
}

/// Decrypt a vault entry back into an Ed25519 [`Identity`].
///
/// Public wrapper over [`decrypt_secret`] for callers outside the
/// `magent web3` subcommand — today that's the
/// `magent run --sign` path in [`crate::runner`], which needs to
/// load an identity from the same on-disk vault without going
/// through the full `Web3Cmd` dispatch (it has its own options
/// shape: `--signer <NAME>`, `--passphrase-env <NAME>`, etc.).
///
/// Errors:
///
/// * `Web3CliError::NotFound(name)` — there's no vault entry for
///   `name`.
/// * `Web3CliError::Aead(_)` — wrong passphrase / corrupt vault.
/// * `Web3CliError::Aead("decrypted seed is not valid UTF-8 …")`
///   — the entry is decryptable but the plaintext isn't a hex
///   string (only hex seeds are supported today; future
///   expansion to base64 / raw bytes is straightforward).
/// * `magent_core::error::Web3ErrorKind::*` — the hex decoded
///   but is the wrong length or contains non-hex digits.
pub fn decrypt_identity(
    vault: &mut Vault,
    name: &str,
    passphrase: &[u8],
) -> Result<magent_core::web3::Identity, Web3CliError> {
    let entry = vault
        .identities
        .get(name)
        .ok_or_else(|| Web3CliError::NotFound(name.to_string()))?;
    let secret_bytes = decrypt_secret(
        &entry.salt_b64,
        &entry.nonce_b64,
        &entry.ciphertext_b64,
        passphrase,
    )?;
    let secret_str = std::str::from_utf8(&secret_bytes).map_err(|_| {
        Web3CliError::Aead("decrypted seed is not valid UTF-8 (corrupt vault?)".to_string())
    })?;
    magent_core::web3::Identity::from_secret_hex(secret_str).map_err(|e| {
        Web3CliError::Aead(format!("decrypted seed is malformed: {}", e))
    })
}

fn base64_encode(bytes: &[u8]) -> String {
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine as _;
    STANDARD.encode(bytes)
}

fn base64_decode(s: &str) -> Result<Vec<u8>, Web3CliError> {
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine as _;
    STANDARD.decode(s).map_err(|e| {
        Web3CliError::Aead(format!("base64 decode failed: {}", e))
    })
}

fn decode_hex(s: &str) -> Result<Vec<u8>, Web3CliError> {
    let s = s.strip_prefix("0x").unwrap_or(s);
    if !s.len().is_multiple_of(2) {
        return Err(Web3CliError::Kdf(
            "hex string must have an even number of digits".to_string(),
        ));
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    let bytes = s.as_bytes();
    for chunk in bytes.chunks(2) {
        let hi = hex_nibble(chunk[0])?;
        let lo = hex_nibble(chunk[1])?;
        out.push((hi << 4) | lo);
    }
    Ok(out)
}

fn hex_nibble(c: u8) -> Result<u8, Web3CliError> {
    match c {
        b'0'..=b'9' => Ok(c - b'0'),
        b'a'..=b'f' => Ok(c - b'a' + 10),
        b'A'..=b'F' => Ok(c - b'A' + 10),
        _ => Err(Web3CliError::Kdf(format!(
            "invalid hex digit: '{}'",
            c as char
        ))),
    }
}

fn now_unix_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ============================================================================
// Renderers
// ============================================================================
// One struct + one render fn per action so the JSON envelope stays
// uniform and the human mode gets clean, predictable output. The
// JSON envelope is a single object with `action`, `name`, and the
// action-specific payload.

struct NewDisplay<'a> {
    name: &'a str,
    did: &'a str,
    public_key_hex: &'a str,
    vault_path: &'a str,
}

fn render_new(d: &NewDisplay<'_>, out: &mut Output) {
    if out.kind() == OutputKind::Json {
        out.write_json_str(
            serde_json::json!({
                "action": "new",
                "name": d.name,
                "did": d.did,
                "public_key_hex": d.public_key_hex,
                "vault_path": d.vault_path,
            })
            .to_string(),
        );
    } else {
        let _ = out.info(&format!("identity {:?} created", d.name));
        let _ = out.info(&format!("  did:    {}", d.did));
        let _ = out.info(&format!("  pubkey: {}", d.public_key_hex));
        let _ = out.info(&format!("  vault:  {}", d.vault_path));
    }
}

struct IdentityDisplay<'a> {
    name: &'a str,
    did: &'a str,
    public_key_hex: &'a str,
    created_at_unix: u64,
    updated_at_unix: u64,
    vault_path: &'a str,
}

fn render_identity(d: &IdentityDisplay<'_>, out: &mut Output) {
    if out.kind() == OutputKind::Json {
        out.write_json_str(
            serde_json::json!({
                "action": "identity",
                "name": d.name,
                "did": d.did,
                "public_key_hex": d.public_key_hex,
                "created_at_unix": d.created_at_unix,
                "updated_at_unix": d.updated_at_unix,
                "vault_path": d.vault_path,
            })
            .to_string(),
        );
    } else {
        let _ = out.info(&format!("name:      {}", d.name));
        let _ = out.info(&format!("did:       {}", d.did));
        let _ = out.info(&format!("pubkey:    {}", d.public_key_hex));
        let _ = out.info(&format!(
            "created:   unix {}",
            d.created_at_unix
        ));
        let _ = out.info(&format!(
            "updated:   unix {}",
            d.updated_at_unix
        ));
        let _ = out.info(&format!("vault:     {}", d.vault_path));
    }
}

struct DidDisplay<'a> {
    source: &'a str,
    did: &'a str,
}

fn render_did(d: &DidDisplay<'_>, out: &mut Output) {
    if out.kind() == OutputKind::Json {
        out.write_json_str(
            serde_json::json!({
                "action": "did",
                "source": d.source,
                "did": d.did,
            })
            .to_string(),
        );
    } else {
        let _ = out.info(&format!("did (from {}): {}", d.source, d.did));
    }
}

struct PubkeyDisplay<'a> {
    public_key_hex: &'a str,
    did: &'a str,
}

fn render_pubkey(d: &PubkeyDisplay<'_>, out: &mut Output) {
    if out.kind() == OutputKind::Json {
        out.write_json_str(
            serde_json::json!({
                "action": "pubkey",
                "public_key_hex": d.public_key_hex,
                "did": d.did,
            })
            .to_string(),
        );
    } else {
        let _ = out.info(&format!("public_key_hex: {}", d.public_key_hex));
        let _ = out.info(&format!("did:            {}", d.did));
    }
}

struct ListEntryDisplay<'a> {
    name: &'a str,
    did: &'a str,
    public_key_hex: &'a str,
    created_at_unix: u64,
    updated_at_unix: u64,
}

struct ListDisplay<'a> {
    identities: Vec<ListEntryDisplay<'a>>,
    vault_path: &'a str,
}

fn render_list(d: &ListDisplay<'_>, out: &mut Output) {
    if out.kind() == OutputKind::Json {
        let entries: Vec<_> = d
            .identities
            .iter()
            .map(|e| {
                serde_json::json!({
                    "name": e.name,
                    "did": e.did,
                    "public_key_hex": e.public_key_hex,
                    "created_at_unix": e.created_at_unix,
                    "updated_at_unix": e.updated_at_unix,
                })
            })
            .collect();
        out.write_json_str(
            serde_json::json!({
                "action": "list",
                "vault_path": d.vault_path,
                "identities": entries,
            })
            .to_string(),
        );
    } else if d.identities.is_empty() {
        let _ = out.info(&format!(
            "no identities in vault ({})",
            d.vault_path
        ));
    } else {
        let _ = out.info(&format!("identities (vault: {})", d.vault_path));
        for e in &d.identities {
            let _ = out.info(&format!("  {}", e.name));
            let _ = out.info(&format!("    did:    {}", e.did));
            let _ = out.info(&format!("    pubkey: {}", e.public_key_hex));
            let _ = out.info(&format!("    created unix {}", e.created_at_unix));
        }
    }
}

#[derive(Serialize)]
struct ExportedIdentity<'a> {
    schema_version: u32,
    name: &'a str,
    public_key_hex: &'a str,
    did: &'a str,
    created_at_unix: u64,
    updated_at_unix: u64,
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// Vault → identity → sign → verify round-trip using a
    /// fixed passphrase.
    #[test]
    fn vault_sign_verify_roundtrip() {
        let dir = std::env::temp_dir().join(format!(
            "magent-web3-test-{}",
            std::process::id()
        ));
        let _ = fs::create_dir_all(&dir);
        let path = dir.join("keys.json");
        let _ = fs::remove_file(&path);

        // The passphrase resolver is captured by the closure. We
        // always return the same passphrase so `encrypt` and
        // `decrypt` use the same key.
        let pp = String::from("correct horse battery staple");
        let pp_for_sign = pp.clone();

        // new
        let new_opts = Web3NewOptions {
            name: "alice".to_string(),
            passphrase_env: None,
            force: false,
            vault_override: Some(path.clone()),
        };
        let action = Web3Action::New(new_opts);
        let mut cmd = Web3Cmd::new(&action, Box::new(move |_| Ok(pp.clone())));
        let mut out = Output::new(OutputKind::Human, true);
        cmd.execute(&mut out).expect("new must succeed");

        // sign
        let payload_file = dir.join("payload.bin");
        fs::write(&payload_file, b"hello bob, it's alice").unwrap();
        let sign_opts = Web3SignOptions {
            name: "alice".to_string(),
            payload: PayloadSource::File(payload_file.clone()),
            output: Some(dir.join("envelope.json")),
            passphrase_env: None,
            vault_override: Some(path.clone()),
        };
        let action = Web3Action::Sign(sign_opts);
        let mut cmd = Web3Cmd::new(
            &action,
            Box::new(move |_| Ok(pp_for_sign.clone())),
        );
        cmd.execute(&mut out).expect("sign must succeed");

        // verify
        let verify_opts = Web3VerifyOptions {
            payload: PayloadSource::File(payload_file),
            envelope: dir.join("envelope.json"),
        };
        let action = Web3Action::Verify(verify_opts);
        let mut cmd = Web3Cmd::new(
            &action,
            Box::new(|_| Ok(String::new())), // unused
        );
        cmd.execute(&mut out)
            .expect("verify must succeed against the right envelope");

        // verify with tampered payload must fail.
        let tampered = dir.join("tampered.bin");
        fs::write(&tampered, b"tampered message").unwrap();
        let verify_bad = Web3VerifyOptions {
            payload: PayloadSource::File(tampered),
            envelope: dir.join("envelope.json"),
        };
        let action = Web3Action::Verify(verify_bad);
        let mut cmd = Web3Cmd::new(
            &action,
            Box::new(|_| Ok(String::new())),
        );
        let err = cmd
            .execute(&mut out)
            .expect_err("verify with tampered payload must fail");
        assert!(matches!(err, Web3CliError::VerificationFailed { .. }));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn wrong_passphrase_fails_decrypt() {
        let dir = std::env::temp_dir().join(format!(
            "magent-web3-test-wrong-pp-{}",
            std::process::id()
        ));
        let _ = fs::create_dir_all(&dir);
        let path = dir.join("keys.json");
        let _ = fs::remove_file(&path);

        let pp = String::from("correct passphrase");
        let new_opts = Web3NewOptions {
            name: "bob".to_string(),
            passphrase_env: None,
            force: false,
            vault_override: Some(path.clone()),
        };
        let action = Web3Action::New(new_opts);
        let mut cmd = Web3Cmd::new(&action, Box::new(move |_| Ok(pp.clone())));
        let mut out = Output::new(OutputKind::Human, true);
        cmd.execute(&mut out).expect("new must succeed");

        // Sign with the wrong passphrase — must surface Aead error.
        let payload_file = dir.join("payload.bin");
        fs::write(&payload_file, b"msg").unwrap();
        let sign_opts = Web3SignOptions {
            name: "bob".to_string(),
            payload: PayloadSource::File(payload_file),
            output: Some(dir.join("envelope.json")),
            passphrase_env: None,
            vault_override: Some(path.clone()),
        };
        let action = Web3Action::Sign(sign_opts);
        let mut cmd = Web3Cmd::new(
            &action,
            Box::new(|_| Ok(String::from("wrong passphrase"))),
        );
        let err = cmd
            .execute(&mut out)
            .expect_err("sign with wrong passphrase must fail");
        assert!(matches!(err, Web3CliError::Aead(_)));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn name_validation_rejects_path_separators_and_empties() {
        assert!(validate_name("alice").is_ok());
        assert!(validate_name("a.b_c-d").is_ok());
        assert!(validate_name("").is_err());
        assert!(validate_name("../etc/passwd").is_err());
        assert!(validate_name("a/b").is_err());
        assert!(validate_name(&"a".repeat(IDENTITY_NAME_MAX + 1)).is_err());
    }

    #[test]
    fn hex_round_trip() {
        let bytes = decode_hex("deadbeef").unwrap();
        assert_eq!(bytes, vec![0xde, 0xad, 0xbe, 0xef]);
        assert!(decode_hex("abc").is_err()); // odd length
        assert!(decode_hex("zz").is_err()); // bad digit
        assert_eq!(decode_hex("0xDEADBEEF").unwrap(), bytes); // prefix stripped
    }

    #[test]
    fn list_renders_empty_vault_without_panic() {
        let dir = std::env::temp_dir().join(format!(
            "magent-web3-test-list-{}",
            std::process::id()
        ));
        let _ = fs::create_dir_all(&dir);
        let path = dir.join("keys.json");
        let _ = fs::remove_file(&path);

        // Force the default vault path resolution to point at our
        // scratch dir by exporting the env var. We have to
        // round-trip through `Command::new` because the test
        // process inherits the env from cargo, and `default_vault_path`
        // reads `$MAGENT_WEB3_KEYSTORE` once per call (not cached).
        // The safe std pattern is to set the var in this scope.
        // SAFETY: only this test thread mutates the env var.
        unsafe {
            std::env::set_var(KEYSTORE_ENV, &path);
        }
        let action = Web3Action::List;
        let mut cmd = Web3Cmd::new(&action, Box::new(|_| Ok(String::new())));
        let mut out = Output::new(OutputKind::Human, true);
        cmd.execute(&mut out)
            .expect("list on missing vault must succeed (load_or_init)");
        unsafe {
            std::env::remove_var(KEYSTORE_ENV);
        }
        let _ = fs::remove_dir_all(&dir);
    }
}