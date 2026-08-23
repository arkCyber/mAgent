//! Configuration loading.
//!
//! We read IMAP/SMTP credentials from environment variables first,
//! then fall back to `~/.config/magent/email-mcp.toml`. Keeping the
//! credential surface in env vars makes the server compatible with
//! the wider MCP ecosystem (Claude Desktop, Cursor, VS Code all
//! support `${ENV_VAR}` substitution in their JSON config).

use std::path::PathBuf;

use serde::Deserialize;

/// Resolved configuration for both the IMAP and SMTP clients.
///
/// All fields are mandatory — partial configuration is rejected at
/// load time so the server fails fast instead of producing confusing
/// mid-flight errors.
#[derive(Debug, Clone)]
pub struct Config {
    /// IMAP server hostname (e.g. `imap.gmail.com`).
    pub imap_host: String,
    /// IMAP server port (typically 993 for TLS).
    pub imap_port: u16,
    /// SMTP server hostname (e.g. `smtp.gmail.com`).
    pub smtp_host: String,
    /// SMTP server port (typically 587 for STARTTLS, 465 for TLS).
    pub smtp_port: u16,
    /// Username for both IMAP and SMTP authentication.
    pub user: String,
    /// Password / app password used for both IMAP and SMTP.
    pub password: String,
}

/// On-disk TOML mirror of [`Config`]. Used only when env vars are
/// missing; ignored entirely otherwise.
#[derive(Debug, Deserialize, Default)]
struct TomlConfig {
    #[serde(default)]
    imap: TomlEndpoint,
    #[serde(default)]
    smtp: TomlEndpoint,
    #[serde(default)]
    user: Option<String>,
    #[serde(default)]
    password: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct TomlEndpoint {
    #[serde(default)]
    host: Option<String>,
    #[serde(default)]
    port: Option<u16>,
}

impl Config {
    /// Load configuration from env vars and `~/.config/magent/email-mcp.toml`.
    pub fn load() -> Result<Self, anyhow::Error> {
        let env_cfg = Self::from_env()?;
        if let Some(env_cfg) = env_cfg {
            return Ok(env_cfg);
        }

        // Env missing — try the TOML fallback.
        if let Some(path) = default_toml_path() {
            if path.exists() {
                let raw = std::fs::read_to_string(&path)?;
                let toml: TomlConfig = toml::from_str(&raw).unwrap_or_default();
                return Self::merge(env_cfg, toml);
            }
        }

        anyhow::bail!(
            "email-mcp: no credentials found. Set IMAP_HOST/IMAP_PORT/IMAP_USER/IMAP_PASSWORD \
             and SMTP_HOST/SMTP_PORT/SMTP_USER/SMTP_PASSWORD env vars, \
             or write ~/.config/magent/email-mcp.toml"
        )
    }

    fn from_env() -> Result<Option<Self>, anyhow::Error> {
        let imap_host = env("IMAP_HOST");
        let smtp_host = env("SMTP_HOST");
        let user = env("IMAP_USER").or_else(|| env("SMTP_USER"));
        let password = env("IMAP_PASSWORD").or_else(|| env("SMTP_PASSWORD"));

        match (imap_host, smtp_host, user, password) {
            (Some(imap_host), Some(smtp_host), Some(user), Some(password)) => Ok(Some(Self {
                imap_host,
                imap_port: env_u16("IMAP_PORT", 993)?,
                smtp_host,
                smtp_port: env_u16("SMTP_PORT", 587)?,
                user,
                password,
            })),
            // If *any* is missing we treat it as "env not configured"
            // and let the caller try the TOML fallback.
            _ => Ok(None),
        }
    }

    fn merge(_env: Option<Self>, toml: TomlConfig) -> Result<Self, anyhow::Error> {
        let imap_host = toml.imap.host.ok_or_else(|| anyhow::anyhow!("email-mcp.toml missing imap.host"))?;
        let smtp_host = toml.smtp.host.ok_or_else(|| anyhow::anyhow!("email-mcp.toml missing smtp.host"))?;
        let user = toml.user.ok_or_else(|| anyhow::anyhow!("email-mcp.toml missing user"))?;
        let password = toml.password.ok_or_else(|| anyhow::anyhow!("email-mcp.toml missing password"))?;
        Ok(Self {
            imap_host,
            imap_port: toml.imap.port.unwrap_or(993),
            smtp_host,
            smtp_port: toml.smtp.port.unwrap_or(587),
            user,
            password,
        })
    }
}

fn env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

fn env_u16(key: &str, default: u16) -> Result<u16, anyhow::Error> {
    match std::env::var(key) {
        Ok(v) if !v.is_empty() => v
            .parse()
            .map_err(|e| anyhow::anyhow!("{key} is not a valid u16: {e}")),
        _ => Ok(default),
    }
}

fn default_toml_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    let mut p = PathBuf::from(home);
    p.push(".config");
    p.push("magent");
    p.push("email-mcp.toml");
    Some(p)
}
