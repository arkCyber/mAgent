//! mAgent MQTT MCP Server
//!
//! A Model Context Protocol (MCP) server that exposes MQTT
//! publish/subscribe tools over the stdio transport. Designed
//! to plug into `magent-core` (via the host gateway) or any
//! MCP-compatible client (Claude Desktop, Cursor, VS Code
//! Copilot, etc.).
//!
//! ## Tools exposed
//!
//! | Tool              | Purpose                                  |
//! |-------------------|------------------------------------------|
//! | `publish_event`   | Publish a UTF-8 payload to a topic       |
//! | `subscribe_topic` | Register a topic filter                  |
//! | `broker_status`   | Diagnostics: broker endpoint + state     |
//!
//! ## Configuration
//!
//! Credentials and broker details are read from environment
//! variables (preferred) or from `~/.config/magent/mqtt-mcp.toml`:
//!
//! ```text
//! MQTT_BROKER_HOST=localhost
//! MQTT_BROKER_PORT=1883
//! MQTT_CLIENT_ID=magent-cli
//! MQTT_KEEP_ALIVE_SECS=30
//! MQTT_USERNAME=...
//! MQTT_PASSWORD=...
//! MQTT_DEFAULT_TOPIC=magent/events
//! MQTT_QOS=1
//! ```
//!
//! ## Transport
//!
//! JSON-RPC 2.0 over newline-delimited stdio. Each line on stdin
//! is one JSON-RPC request; each line on stdout is one JSON-RPC
//! response (or notification).

#![deny(unsafe_op_in_unsafe_fn)]
#![warn(missing_docs)]

mod config;
mod jsonrpc;
mod mqtt_client;
mod tools;

use std::sync::Arc;

use tokio::io::{self, AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::config::Config;
use crate::mqtt_client::MqttClient;
use crate::tools::ToolRegistry;

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> anyhow::Result<()> {
    // Initialise logging to stderr so it never interleaves with
    // the JSON-RPC stream on stdout.
    if env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_timestamp_millis()
        .try_init()
        .is_ok()
    {
        log::debug!("mqtt-mcp: logging initialised");
    }

    // Support `--show-config` for operator diagnostics. We print
    // to stdout (not stderr) because that's what gets captured
    // when an MCP client introspects the server. Only one flag
    // is honoured — the rest of the CLI surface is reserved for
    // future expansion.
    if let Some(arg) = std::env::args().nth(1) {
        match arg.as_str() {
            "--show-config" => {
                let cfg = Config::load()?;
                println!("{}", serde_json::to_string_pretty(&cfg)?);
                return Ok(());
            }
            "--help" | "-h" => {
                println!(
                    "magent-mqtt-mcp {}\n\n\
                     MQTT 3.1.1 MCP server. Reads JSON-RPC 2.0 requests\n\
                     on stdin and writes responses on stdout.\n\n\
                     Environment overrides: MQTT_BROKER_HOST,\n\
                     MQTT_BROKER_PORT, MQTT_CLIENT_ID,\n\
                     MQTT_KEEP_ALIVE_SECS, MQTT_USERNAME,\n\
                     MQTT_PASSWORD, MQTT_DEFAULT_TOPIC, MQTT_QOS\n\n\
                     Config file: ~/.config/magent/mqtt-mcp.toml\n\n\
                     Flags:\n\
                     --show-config  print resolved config and exit\n\
                     --help, -h     print this message",
                    env!("CARGO_PKG_VERSION")
                );
                return Ok(());
            }
            other => {
                anyhow::bail!("unknown argument: {} (try --help)", other);
            }
        }
    }

    let cfg = Config::load()?;
    log::info!(
        "mqtt-mcp: connecting to broker at {} (client_id={}, qos_default={})",
        cfg.broker_endpoint(),
        cfg.client_id,
        cfg.qos_default
    );

    let client = MqttClient::connect(cfg).await?;
    tools::install_client(client);

    let registry = Arc::new(ToolRegistry::new());
    run_stdio_loop(registry).await
}

/// Drive the JSON-RPC request loop until EOF on stdin.
async fn run_stdio_loop(registry: Arc<ToolRegistry>) -> anyhow::Result<()> {
    let stdin = io::stdin();
    let mut stdout = io::stdout();
    let mut reader = BufReader::new(stdin);
    let mut line = String::new();

    loop {
        line.clear();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            // EOF — the parent process closed stdin. Exit cleanly.
            log::info!("mqtt-mcp: stdin EOF, shutting down");
            break;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let reply = jsonrpc::dispatch(trimmed, &registry).await;
        match reply {
            Ok(Some(value)) => {
                let mut buf = serde_json::to_vec(&value)?;
                buf.push(b'\n');
                stdout.write_all(&buf).await?;
                stdout.flush().await?;
            }
            Ok(None) => {
                // Notification — no reply.
            }
            Err(e) => {
                // We weren't able to produce a JSON-RPC envelope at
                // all (parse failure, internal error). Surface a
                // generic parse-error envelope with id=null so the
                // client knows the request was rejected.
                let envelope = serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": null,
                    "error": {
                        "code": -32700,
                        "message": e.to_string(),
                    }
                });
                let mut buf = serde_json::to_vec(&envelope)?;
                buf.push(b'\n');
                stdout.write_all(&buf).await?;
                stdout.flush().await?;
            }
        }
    }
    Ok(())
}
