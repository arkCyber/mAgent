//! Runtime configuration for the MQTT MCP server.
//!
//! Loaded from a TOML file (`~/.config/magent/mqtt-mcp.toml` by
//! default) and overridable via environment variables. The split
//! mirrors `email-mcp` so operators can swap backends with a single
//! `cp`.
//!
//! ## File format
//!
//! ```toml
//! broker_host = "localhost"
//! broker_port = 1883
//! client_id = "magent-cli"
//! keep_alive_secs = 30
//! default_topic = "magent/events"
//! username = ""
//! password = ""
//! qos_default = 1
//! ```
//!
//! ## Environment overrides
//!
//! ```text
//! MQTT_BROKER_HOST, MQTT_BROKER_PORT, MQTT_CLIENT_ID,
//! MQTT_KEEP_ALIVE_SECS, MQTT_USERNAME, MQTT_PASSWORD,
//! MQTT_DEFAULT_TOPIC, MQTT_QOS
//! ```

use std::env;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Errors returned by [`Config::load`].
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// TOML parse failure on the config file.
    #[error("could not parse config file {path}: {source}")]
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },
    /// I/O failure on the config file.
    #[error("could not read config file {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    /// `MQTT_BROKER_PORT` could not be parsed as a u16.
    #[error("MQTT_BROKER_PORT={value:?} is not a valid port number")]
    InvalidPort { value: String },
    /// `MQTT_KEEP_ALIVE_SECS` could not be parsed as a u16.
    #[error("MQTT_KEEP_ALIVE_SECS={value:?} is not a valid non-negative integer")]
    InvalidKeepAlive { value: String },
    /// `MQTT_QOS` could not be parsed as a 0/1/2 value.
    #[error("MQTT_QOS={value:?} is not 0, 1, or 2")]
    InvalidQos { value: String },
}

/// Resolved runtime configuration for the MQTT client.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Config {
    /// Broker hostname or IP. Empty string is allowed but rejected
    /// at connect-time by `rumqttc`.
    pub broker_host: String,
    /// Broker port. Defaults to the unencrypted MQTT port (1883).
    pub broker_port: u16,
    /// MQTT client identifier. `rumqttc` allows an empty string
    /// (the broker assigns one) but we default to a stable one so
    /// operators can identify mAgent sessions in `mosquitto`'s
    /// `$SYS` introspection.
    pub client_id: String,
    /// PINGREQ interval in seconds.
    pub keep_alive_secs: u16,
    /// Default topic used by `publish_event` when the LLM doesn't
    /// specify one.
    pub default_topic: String,
    /// Optional username for broker auth.
    pub username: String,
    /// Optional password for broker auth.
    pub password: String,
    /// Default QoS level for publishes (0 / 1 / 2).
    pub qos_default: u8,
}

impl Config {
    /// Construct a config with sensible defaults. Useful for tests
    /// and the `--help` / `--show-config` paths.
    pub fn defaults() -> Self {
        Self {
            broker_host: "localhost".to_string(),
            broker_port: 1883,
            client_id: "magent-cli".to_string(),
            keep_alive_secs: 30,
            default_topic: "magent/events".to_string(),
            username: String::new(),
            password: String::new(),
            qos_default: 1,
        }
    }

    /// Load configuration from `~/.config/magent/mqtt-mcp.toml`,
    /// then layer environment-variable overrides on top.
    ///
    /// A missing config file is **not** an error — we treat it as
    /// "use defaults, then apply env". This matches the
    /// `email-mcp` UX and lets operators deploy with env-only
    /// configs in containers.
    pub fn load() -> Result<Self, ConfigError> {
        let path = default_config_path();
        let mut cfg = if path.exists() {
            let body = std::fs::read_to_string(&path).map_err(|source| ConfigError::Io {
                path: path.clone(),
                source,
            })?;
            // Merge: defaults -> file values. Missing keys stay at
            // their default; we don't fail on partial files.
            let mut base = Self::defaults();
            let file: Self = toml::from_str(&body).map_err(|source| ConfigError::Parse {
                path: path.clone(),
                source,
            })?;
            merge(&mut base, &file);
            base
        } else {
            Self::defaults()
        };
        apply_env_overrides(&mut cfg)?;
        Ok(cfg)
    }

    /// Render the broker host+port as a string suitable for log
    /// lines. Keeps credentials out of the output (we never log
    /// username/password).
    pub fn broker_endpoint(&self) -> String {
        format!("{}:{}", self.broker_host, self.broker_port)
    }
}

impl Default for Config {
    fn default() -> Self {
        Self::defaults()
    }
}

/// Compute `~/.config/magent/mqtt-mcp.toml`. `None` when the home
/// directory can't be resolved (e.g. `$HOME` unset in a container
/// without a user).
fn default_config_path() -> PathBuf {
    env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
        .join(".config")
        .join("magent")
        .join("mqtt-mcp.toml")
}

/// Layer `override_` on top of `base`. Used to merge the parsed
/// TOML file with the in-memory defaults — missing keys in the
/// file stay at their default value.
fn merge(base: &mut Config, override_: &Config) {
    if !override_.broker_host.is_empty() {
        base.broker_host = override_.broker_host.clone();
    }
    if override_.broker_port != 0 {
        base.broker_port = override_.broker_port;
    }
    if !override_.client_id.is_empty() {
        base.client_id = override_.client_id.clone();
    }
    if override_.keep_alive_secs != 0 {
        base.keep_alive_secs = override_.keep_alive_secs;
    }
    if !override_.default_topic.is_empty() {
        base.default_topic = override_.default_topic.clone();
    }
    if !override_.username.is_empty() {
        base.username = override_.username.clone();
    }
    if !override_.password.is_empty() {
        base.password = override_.password.clone();
    }
    if override_.qos_default <= 2 {
        base.qos_default = override_.qos_default;
    }
}

/// Apply `MQTT_*` env overrides on top of the file-loaded config.
fn apply_env_overrides(cfg: &mut Config) -> Result<(), ConfigError> {
    if let Ok(v) = env::var("MQTT_BROKER_HOST") {
        cfg.broker_host = v;
    }
    if let Ok(v) = env::var("MQTT_BROKER_PORT") {
        cfg.broker_port = v
            .parse()
            .map_err(|_| ConfigError::InvalidPort { value: v })?;
    }
    if let Ok(v) = env::var("MQTT_CLIENT_ID") {
        cfg.client_id = v;
    }
    if let Ok(v) = env::var("MQTT_KEEP_ALIVE_SECS") {
        cfg.keep_alive_secs = v
            .parse()
            .map_err(|_| ConfigError::InvalidKeepAlive { value: v })?;
    }
    if let Ok(v) = env::var("MQTT_DEFAULT_TOPIC") {
        cfg.default_topic = v;
    }
    if let Ok(v) = env::var("MQTT_USERNAME") {
        cfg.username = v;
    }
    if let Ok(v) = env::var("MQTT_PASSWORD") {
        cfg.password = v;
    }
    if let Ok(v) = env::var("MQTT_QOS") {
        let parsed: u8 = v.parse().map_err(|_| ConfigError::InvalidQos { value: v })?;
        if parsed > 2 {
            return Err(ConfigError::InvalidQos { value: parsed.to_string() });
        }
        cfg.qos_default = parsed;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_sensible() {
        let c = Config::defaults();
        assert_eq!(c.broker_port, 1883);
        assert_eq!(c.qos_default, 1);
        assert!(!c.client_id.is_empty());
    }

    #[test]
    fn merge_preserves_unset_keys() {
        let mut base = Config::defaults();
        let partial = Config {
            broker_host: "broker.example.com".to_string(),
            broker_port: 8883,
            ..Config::defaults()
        };
        merge(&mut base, &partial);
        assert_eq!(base.broker_host, "broker.example.com");
        assert_eq!(base.broker_port, 8883);
        // Keep_alive_secs is 0 in the override — we should leave
        // the default (30) intact.
        assert_eq!(base.keep_alive_secs, 30);
    }

    #[test]
    fn broker_endpoint_redacts_creds() {
        let mut c = Config::defaults();
        c.username = "secret".into();
        c.password = "hunter2".into();
        let endpoint = c.broker_endpoint();
        assert!(!endpoint.contains("secret"));
        assert!(!endpoint.contains("hunter2"));
    }
}
