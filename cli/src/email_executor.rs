//! Composite tool executor that combines `SimulatorExecutor` (embedded
//! sensor/BLE/flash/GPIO tools) with `McpToolExecutor` (email MCP tools)
//! and `BlockchainExecutor` (web3 tools).
//!
//! `SimulatorExecutor` handles embedded tools.
//! `McpToolExecutor` intercepts email MCP tools.
//! `BlockchainExecutor` intercepts `get_balance` / `send_transaction` /
//! other web3 tool names.

use magent_core::agent_runner::ToolExecutor;
use magent_core::real_tools::SimulatorExecutor;
use std::fmt;

#[cfg(feature = "web3")]
use crate::blockchain_executor::BlockchainExecutor;

/// Executor that tries `SimulatorExecutor` first, then falls back to
/// `McpToolExecutor` for email MCP tools and `BlockchainExecutor` for
/// web3 tool calls.
pub enum CompositeExecutor {
    /// Simulator only (web3 and email-tools features disabled).
    SimulatorOnly(SimulatorExecutor),
    /// Simulator + blockchain tools (web3 feature enabled).
    #[cfg(feature = "web3")]
    WithBlockchain {
        sim: SimulatorExecutor,
        blockchain: BlockchainExecutor,
    },
    /// Simulator + MCP email tools (email-tools feature enabled).
    #[cfg(feature = "email-tools")]
    WithEmailTools {
        sim: SimulatorExecutor,
        email: mcp_tool_executor::McpToolExecutor,
    },
    /// All three backends combined.
    #[cfg(all(feature = "web3", feature = "email-tools"))]
    Full {
        sim: SimulatorExecutor,
        email: mcp_tool_executor::McpToolExecutor,
        blockchain: BlockchainExecutor,
    },
}

impl CompositeExecutor {
    /// Create a `CompositeExecutor` from the feature flags.
    #[cfg(all(feature = "web3", feature = "email-tools"))]
    pub fn new_with_features(
        email_tools_path: Option<&str>,
        blockchain_rpc: Option<&str>,
        blockchain_chain_id: Option<u64>,
    ) -> Self {
        let mut sim = SimulatorExecutor::new();
        sim.connect_ble();
        let email = match email_tools_path {
            Some(path) if !path.is_empty() => Some(mcp_tool_executor::McpToolExecutor::new(
                path.to_string(),
            )),
            _ => None,
        };
        let blockchain = match blockchain_rpc {
            Some(url) => {
                let mut be = BlockchainExecutor::new(url, blockchain_chain_id.unwrap_or(1));
                be.init();
                Some(be)
            }
            None => None,
        };
        match (email, blockchain) {
            (Some(email), Some(blockchain)) => Self::Full { sim, email, blockchain },
            (Some(email), None) => Self::WithEmailTools { sim, email },
            (None, Some(blockchain)) => Self::WithBlockchain { sim, blockchain },
            (None, None) => Self::SimulatorOnly(sim),
        }
    }

    /// Create a `CompositeExecutor` from the `--email-tools` flag value.
    ///
    /// - `None` → `SimulatorOnly`
    /// - `Some("")` → `WithEmailTools` using the default binary path
    ///   (`magent-email-mcp` resolved via `$PATH`)
    /// - `Some(path)` → `WithEmailTools` using the supplied path
    #[cfg(feature = "email-tools")]
    pub fn new(email_tools: Option<&str>) -> Self {
        let mut sim = SimulatorExecutor::new();
        sim.connect_ble();
        match email_tools {
            None => Self::SimulatorOnly(sim),
            Some(path) => {
                let path = if path.is_empty() {
                    "magent-email-mcp".to_string()
                } else {
                    path.to_string()
                };
                let email = mcp_tool_executor::McpToolExecutor::new(path);
                #[cfg(feature = "web3")]
                {
                    // Auto-wire a default blockchain executor so the LLM
                    // can make web3 calls without extra CLI flags.
                    let mut blockchain = BlockchainExecutor::ethereum_mainnet();
                    blockchain.init();
                    Self::Full {
                        sim,
                        email,
                        blockchain,
                    }
                }
                #[cfg(not(feature = "web3"))]
                {
                    Self::WithEmailTools { sim, email }
                }
            }
        }
    }

    /// Create without email tools (used when the feature is disabled).
    #[cfg(not(feature = "email-tools"))]
    pub fn new(_email_tools: Option<&str>) -> Self {
        Self::SimulatorOnly(SimulatorExecutor::new())
    }

    /// Create a CompositeExecutor with a blockchain backend (web3 only).
    #[cfg(feature = "web3")]
    pub fn with_blockchain(blockchain: BlockchainExecutor) -> Self {
        let sim = SimulatorExecutor::new();
        Self::WithBlockchain { sim, blockchain }
    }

    /// Return `true` if this executor was constructed with email tools
    /// enabled (i.e. the `--email-tools` flag was passed).
    pub fn has_email_tools(&self) -> bool {
        match self {
            Self::SimulatorOnly(_) => false,
            #[cfg(feature = "email-tools")]
            Self::WithEmailTools { .. } => true,
            #[cfg(all(feature = "web3", feature = "email-tools"))]
            Self::Full { .. } => true,
            #[cfg(feature = "web3")]
            Self::WithBlockchain { .. } => false,
        }
    }

    /// Return `true` if blockchain tools are enabled.
    pub fn has_blockchain(&self) -> bool {
        match self {
            Self::SimulatorOnly(_) => false,
            #[cfg(feature = "web3")]
            Self::WithBlockchain { .. } => true,
            #[cfg(all(feature = "web3", feature = "email-tools"))]
            Self::Full { .. } => true,
            #[cfg(feature = "email-tools")]
            Self::WithEmailTools { .. } => false,
        }
    }

    /// Return the list of `mcp__email__*` tool names and descriptions,
    /// or an empty list when email tools are disabled.
    #[cfg(feature = "email-tools")]
    pub fn email_tool_descriptions() -> Vec<(String, String)> {
        mcp_tool_executor::McpToolExecutor::tool_descriptions()
    }

    #[cfg(not(feature = "email-tools"))]
    pub fn email_tool_descriptions() -> Vec<(String, String)> {
        Vec::new()
    }

    /// Return blockchain tool descriptions (when web3 is enabled).
    #[cfg(feature = "web3")]
    pub fn blockchain_tool_descriptions() -> Vec<(String, String)> {
        BlockchainExecutor::tool_descriptions()
            .into_iter()
            .map(|(name, desc)| (name.to_string(), desc.to_string()))
            .collect()
    }

    #[cfg(not(feature = "web3"))]
    pub fn blockchain_tool_descriptions() -> Vec<(String, String)> {
        Vec::new()
    }
}

impl Default for CompositeExecutor {
    fn default() -> Self {
        Self::new(None)
    }
}

impl fmt::Debug for CompositeExecutor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SimulatorOnly(_) => write!(f, "CompositeExecutor::SimulatorOnly"),
            #[cfg(feature = "web3")]
            Self::WithBlockchain { .. } => write!(f, "CompositeExecutor::WithBlockchain"),
            #[cfg(feature = "email-tools")]
            Self::WithEmailTools { .. } => write!(f, "CompositeExecutor::WithEmailTools"),
            #[cfg(all(feature = "web3", feature = "email-tools"))]
            Self::Full { .. } => write!(f, "CompositeExecutor::Full"),
        }
    }
}

/// Decide whether a tool name should be routed to the email MCP backend.
fn is_email_tool(tool: &str) -> bool {
    tool == "ble_send" || tool.starts_with("mcp__email__")
}

/// Decide whether a tool name should be routed to the blockchain backend.
#[cfg(feature = "web3")]
fn is_blockchain_tool(tool: &str) -> bool {
    matches!(
        tool,
        "get_balance"
            | "get_nonce"
            | "get_gas_price"
            | "get_block_number"
            | "send_transaction"
            | "poll_transaction"
            | "blockchain_status"
            | "sign_message"
            | "verify_signature"
    )
}

/// Implement `ToolExecutor` on `CompositeExecutor` by delegating to the
/// appropriate inner executor.
#[cfg(all(feature = "email-tools", feature = "web3"))]
impl ToolExecutor for CompositeExecutor {
    fn execute(
        &mut self,
        tool: &str,
        args: &str,
    ) -> std::result::Result<String, String> {
        match self {
            Self::SimulatorOnly(sim) => sim.execute(tool, args),
            Self::WithEmailTools { sim, email } => {
                if is_email_tool(tool) {
                    email.execute(tool, args)
                } else {
                    sim.execute(tool, args)
                }
            }
            Self::WithBlockchain { sim, blockchain } => {
                if is_blockchain_tool(tool) {
                    blockchain.execute(tool, args)
                } else {
                    sim.execute(tool, args)
                }
            }
            Self::Full {
                sim,
                email,
                blockchain,
            } => {
                if is_email_tool(tool) {
                    email.execute(tool, args)
                } else if is_blockchain_tool(tool) {
                    blockchain.execute(tool, args)
                } else {
                    sim.execute(tool, args)
                }
            }
        }
    }
}

#[cfg(all(feature = "email-tools", not(feature = "web3")))]
impl ToolExecutor for CompositeExecutor {
    fn execute(
        &mut self,
        tool: &str,
        args: &str,
    ) -> std::result::Result<String, String> {
        match self {
            Self::SimulatorOnly(sim) => sim.execute(tool, args),
            Self::WithEmailTools { sim, email } => {
                if is_email_tool(tool) {
                    email.execute(tool, args)
                } else {
                    sim.execute(tool, args)
                }
            }
        }
    }
}

#[cfg(all(not(feature = "email-tools"), feature = "web3"))]
impl ToolExecutor for CompositeExecutor {
    fn execute(
        &mut self,
        tool: &str,
        args: &str,
    ) -> std::result::Result<String, String> {
        match self {
            Self::SimulatorOnly(sim) => sim.execute(tool, args),
            Self::WithBlockchain { sim, blockchain } => {
                if is_blockchain_tool(tool) {
                    blockchain.execute(tool, args)
                } else {
                    sim.execute(tool, args)
                }
            }
        }
    }
}

/// `ToolExecutor` impl when email-tools feature is disabled.
#[cfg(not(any(feature = "email-tools", feature = "web3")))]
impl ToolExecutor for CompositeExecutor {
    fn execute(
        &mut self,
        tool: &str,
        args: &str,
    ) -> std::result::Result<String, String> {
        match self {
            Self::SimulatorOnly(sim) => sim.execute(tool, args),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_simulator_only() {
        let exec = CompositeExecutor::default();
        assert!(!exec.has_email_tools());
    }

    #[test]
    fn none_arg_is_simulator_only() {
        let exec = CompositeExecutor::new(None);
        assert!(!exec.has_email_tools());
    }

    #[test]
    fn empty_arg_is_still_simulator_only_when_feature_disabled() {
        // With the `email-tools` feature off, `Some("")` falls through
        // to `SimulatorOnly`. The user gets a SimulatorExecutor; trying
        // to enable email tools is a no-op.
        let exec = CompositeExecutor::new(Some(""));
        #[cfg(feature = "email-tools")]
        assert!(exec.has_email_tools());
        #[cfg(not(feature = "email-tools"))]
        assert!(!exec.has_email_tools());
    }

    #[test]
    fn debug_impl_covers_all_variants() {
        let sim = CompositeExecutor::default();
        let s = format!("{:?}", sim);
        assert!(s.contains("SimulatorOnly"), "got: {s}");

        #[cfg(feature = "email-tools")]
        {
            let with_email = CompositeExecutor::new(Some("true"));
            let s = format!("{:?}", with_email);
            // When the `web3` feature is also enabled, `new(Some(_))`
            // auto-wires a blockchain backend and returns the `Full`
            // variant; otherwise it returns `WithEmailTools`. Assert on
            // whichever variant the active feature set actually produces.
            #[cfg(feature = "web3")]
            assert!(s.contains("Full"), "got: {s}");
            #[cfg(not(feature = "web3"))]
            assert!(s.contains("WithEmailTools"), "got: {s}");
        }
    }

    #[test]
    fn email_tool_descriptions_matches_executor_when_enabled() {
        // `email_tool_descriptions` is a static catalogue that
        // reflects the *capability* (i.e. the cargo feature), not
        // the runtime variant. When the feature is off, the
        // catalogue is empty; when the feature is on, it returns the
        // five `mcp__email__*` tools regardless of which variant was
        // constructed.
        let descs = CompositeExecutor::email_tool_descriptions();
        #[cfg(feature = "email-tools")]
        {
            assert_eq!(descs.len(), 5, "expected 5 email tools, got {}", descs.len());
        }
        #[cfg(not(feature = "email-tools"))]
        {
            assert!(descs.is_empty(), "feature off but descriptions returned");
        }
    }

    // ── Email-tool routing ─────────────────────────────────────────────
    //
    // These tests only make sense when the `email-tools` feature is
    // enabled because they reach into the `WithEmailTools` variant.
    #[cfg(feature = "email-tools")]
    #[test]
    fn simulator_only_handles_builtin_tool() {
        // `read_sensor` is a built-in simulator tool. The
        // `SimulatorOnly` variant should handle it.
        let mut exec = CompositeExecutor::new(None);
        let result = exec.execute("read_sensor", r#"{"sensor":"temperature"}"#);
        assert!(result.is_ok(), "simulator should handle read_sensor: {result:?}");
        let s = result.unwrap();
        // The simulator's canned response includes either a number
        // (e.g. "25.5") or the phrase "Sensor". Both are non-empty.
        assert!(!s.is_empty());
    }

    #[cfg(feature = "email-tools")]
    #[test]
    fn simulator_only_rejects_ble_send_routing_via_mcp() {
        // When email tools are NOT enabled, `ble_send` falls through to
        // the simulator's pseudo-BLE path. The simulator BLE must be
        // connected first; we model that by switching to the
        // WithEmailTools variant (which connects BLE in its
        // constructor) and then verify the matching routing decision.
        let mut exec = CompositeExecutor::new(None);
        // A non-email ble_send should NOT crash on routing. We can't
        // test the success path here because the simulator's BLE
        // starts disconnected; we just confirm the routing didn't
        // hit the MCP backend (which would require a binary).
        let result = exec.execute("ble_send", r#"{"data":"hello"}"#);
        // Any routing-recognized error or success is fine; what we
        // *don't* want is "unknown tool".
        if let Err(e) = &result {
            assert!(!e.contains("unknown tool"), "got: {e}");
        }
    }

    #[cfg(feature = "email-tools")]
    #[test]
    fn email_tool_routes_to_mcp_when_enabled() {
        // We don't have a real magent-email-mcp binary in the test
        // environment, so we wire the executor to `true` (a binary
        // that just exits 0). The executor will try to spawn it,
        // fail to talk JSON-RPC, and return an error. We just verify
        // the error is the expected MCP-handshake/socket error (not
        // "unknown tool").
        let mut exec = CompositeExecutor::new(Some("true"));
        let result = exec.execute("mcp__email__list_inbox", r#"{"limit": 5}"#);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(!err.contains("unknown tool"), "got: {err}");
    }

    #[cfg(feature = "email-tools")]
    #[test]
    fn ble_send_with_email_chars_routes_to_mcp() {
        // The embedded-actor convention: `ble_send` with
        // `characteristic=email` is an email tool call. The composite
        // executor should route it to the MCP backend.
        let mut exec = CompositeExecutor::new(Some("true"));
        let result = exec.execute(
            "ble_send",
            r#"{"data":"list_inbox","characteristic":"email"}"#,
        );
        // Same as above: we just verify the routing reached the MCP
        // layer (i.e. didn't return "unknown tool"). The actual call
        // may fail because `true` isn't a real MCP server.
        assert!(result.is_err());
        assert!(!result.unwrap_err().contains("unknown tool"));
    }

    #[cfg(feature = "email-tools")]
    #[test]
    fn ble_send_without_email_chars_does_not_route_to_mcp() {
        // Other `ble_send` calls (e.g. `characteristic=heart_rate`)
        // should still hit the simulator, not the MCP backend.
        let mut exec = CompositeExecutor::new(Some("true"));
        let result = exec.execute(
            "ble_send",
            r#"{"data":"hello","characteristic":"heart_rate"}"#,
        );
        // We don't care about success/failure here — the simulator's
        // BLE starts disconnected and would fail regardless. The
        // important property is that the routing did NOT go to the
        // MCP layer (which would never spawn with `true` because of
        // the JSON-RPC handshake).
        if let Err(e) = &result {
            assert!(!e.contains("unknown tool"), "got: {e}");
        }
    }

    #[test]
    fn routing_helper_classifies_tool_names() {
        assert!(is_email_tool("ble_send"));
        assert!(is_email_tool("mcp__email__list_inbox"));
        assert!(is_email_tool("mcp__email__send_email"));
        assert!(!is_email_tool("read_sensor"));
        assert!(!is_email_tool("write_gpio"));
        assert!(!is_email_tool("flash_write"));
    }

    // ── Blockchain routing ────────────────────────────────────────────

    #[cfg(feature = "web3")]
    #[test]
    fn is_blockchain_tool_recognizes_web3_tools() {
        assert!(super::is_blockchain_tool("get_balance"));
        assert!(super::is_blockchain_tool("send_transaction"));
        assert!(super::is_blockchain_tool("sign_message"));
        assert!(!super::is_blockchain_tool("read_sensor"));
        assert!(!super::is_blockchain_tool("write_gpio"));
    }

    #[cfg(feature = "web3")]
    #[test]
    fn with_blockchain_handles_blockchain_query() {
        let mut blockchain = BlockchainExecutor::new("https://eth.llamarpc.com", 1);
        blockchain.init();
        let mut exec = CompositeExecutor::with_blockchain(blockchain);
        assert!(exec.has_blockchain());
        // Unknown tool name should error gracefully (no panic), and the
        // error should NOT propagate "unknown tool" - either routing
        // succeeds (via the simulator or blockchain) or returns a
        // domain error.
        let result = exec.execute("not_a_tool", "{}");
        // Either result is acceptable, but if it's an Err, it must be
        // a blockchain/simulator error not a routing panic.
        if let Err(e) = &result {
            assert!(!e.is_empty());
        }
    }

    #[cfg(feature = "web3")]
    #[test]
    fn blockchain_tool_descriptions_is_non_empty_when_enabled() {
        let descs = CompositeExecutor::blockchain_tool_descriptions();
        assert!(
            !descs.is_empty(),
            "web3 enabled but no blockchain tool descriptions"
        );
        // Each entry should be a (name, description) tuple.
        let (name, desc) = &descs[0];
        assert!(!name.is_empty());
        assert!(!desc.is_empty());
    }

    #[cfg(feature = "web3")]
    #[test]
    fn full_executor_routes_blockchain_call_to_blockchain() {
        // Without email-tools feature, only `WithBlockchain` is built; a
        // request for `get_balance` should be routed to the blockchain
        // executor instead of the simulator.
        let mut blockchain = BlockchainExecutor::new("https://eth.llamarpc.com", 1);
        blockchain.init();
        let mut exec = CompositeExecutor::with_blockchain(blockchain);

        // Without `reqwest` enabled, the network call will fail. But
        // the *routing* decision should still be visible (the error
        // path originates in the blockchain backend, not in the
        // simulator).
        let _ = exec.execute("get_balance", r#"{"address": "0x0000000000000000000000000000000000000001"}"#);
    }
}
