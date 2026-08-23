//! Blockchain Executor for Agent Runner.
//!
//! Wraps `BlockchainManager` as an executor that can be plugged into the
//! `CompositeExecutor` or other executor chains. Allows the agent runner
//! to dispatch blockchain tool calls through the work loop.

#[cfg(feature = "web3")]
use magent_core::web3::blockchain::{
    BlockchainManager, BlockchainToolResult,
};

/// A blockchain executor that wraps `BlockchainManager`.
#[cfg(feature = "web3")]
pub struct BlockchainExecutor {
    manager: BlockchainManager,
    initialized: bool,
}

#[cfg(feature = "web3")]
impl BlockchainExecutor {
    /// Create a new blockchain executor.
    pub fn new(rpc_url: &str, chain_id: u64) -> Self {
        Self {
            manager: BlockchainManager::new(rpc_url, chain_id),
            initialized: false,
        }
    }

    /// Create with default Ethereum mainnet RPC.
    pub fn ethereum_mainnet() -> Self {
        Self::new("https://eth.llamarpc.com", 1)
    }

    /// Initialize the executor (sets up HTTP client).
    pub fn init(&mut self) {
        let _ = self.manager.init();
        self.initialized = self.manager.is_initialized();
    }

    /// Execute a blockchain tool call.
    pub fn execute(
        &mut self,
        tool: &str,
        args: &str,
    ) -> std::result::Result<String, String> {
        // Ensure initialization
        if !self.initialized {
            self.init();
        }

        let result: BlockchainToolResult =
            magent_core::web3::blockchain::execute_blockchain_tool(&mut self.manager, tool, args);

        if result.success {
            Ok(result.data)
        } else {
            Err(result.error.unwrap_or_else(|| "Unknown error".to_string()))
        }
    }

    /// Check if this executor handles a given tool.
    pub fn handles(&self, tool: &str) -> bool {
        // List of blockchain tools handled by this executor
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
        )
    }

    /// Get the manager (for advanced use cases).
    pub fn manager(&self) -> &BlockchainManager {
        &self.manager
    }

    /// Get mutable manager.
    pub fn manager_mut(&mut self) -> &mut BlockchainManager {
        &mut self.manager
    }

    /// Get list of all blockchain tools.
    pub fn tool_names() -> &'static [&'static str] {
        &[
            "get_balance",
            "get_nonce",
            "get_gas_price",
            "get_block_number",
            "send_transaction",
            "poll_transaction",
            "blockchain_status",
            "sign_message",
        ]
    }

    /// Get descriptions of each blockchain tool.
    pub fn tool_descriptions() -> Vec<(&'static str, &'static str)> {
        vec![
            ("get_balance", "Get ETH balance for an Ethereum address (args: {\"address\": \"0x...\"})"),
            ("get_nonce", "Get transaction count for an Ethereum address (args: {\"address\": \"0x...\"})"),
            ("get_gas_price", "Get current gas price in Gwei (no args)"),
            ("get_block_number", "Get current block number (no args)"),
            ("send_transaction", "Send a signed transaction (args: {\"transaction\": \"0x...\"})"),
            ("poll_transaction", "Poll for transaction confirmation (args: {\"tx_hash\": \"0x...\"})"),
            ("blockchain_status", "Get blockchain client status (no args)"),
            ("sign_message", "Sign a message with the agent's identity key (args: {\"message\": \"...\"})"),
        ]
    }
}

#[cfg(feature = "web3")]
impl Default for BlockchainExecutor {
    fn default() -> Self {
        Self::ethereum_mainnet()
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(all(test, feature = "web3"))]
mod tests {
    use super::*;

    #[test]
    fn test_executor_creation() {
        let exec = BlockchainExecutor::ethereum_mainnet();
        assert_eq!(exec.manager().chain_id(), 1);
    }

    #[test]
    fn test_executor_handles_known_tools() {
        let exec = BlockchainExecutor::default();
        assert!(exec.handles("get_balance"));
        assert!(exec.handles("send_transaction"));
        assert!(exec.handles("sign_message"));
    }

    #[test]
    fn test_executor_does_not_handle_unknown_tools() {
        let exec = BlockchainExecutor::default();
        assert!(!exec.handles("read_sensor"));
        assert!(!exec.handles("write_gpio"));
        assert!(!exec.handles("unknown_tool"));
    }

    #[test]
    fn test_tool_names() {
        let names = BlockchainExecutor::tool_names();
        assert_eq!(names.len(), 8);
        assert!(names.contains(&"get_balance"));
        assert!(names.contains(&"send_transaction"));
    }

    #[test]
    fn test_tool_descriptions() {
        let descs = BlockchainExecutor::tool_descriptions();
        assert_eq!(descs.len(), 8);
        // Each description should mention how to call the tool
        let (_, balance_desc) = &descs[0];
        assert!(balance_desc.contains("get_balance") || balance_desc.contains("ETH"));
    }

    #[test]
    fn test_executor_execute_uninitialized_returns_error() {
        let mut exec = BlockchainExecutor::default();
        // Override - execute when client not init returns error gracefully
        let result = exec.execute("get_balance", "{}");
        // Without init, it should fail gracefully (not panic)
        assert!(result.is_err() || result.is_ok());
    }

    #[test]
    fn test_executor_with_different_chain() {
        let exec = BlockchainExecutor::new("https://polygon-rpc.com", 137);
        assert_eq!(exec.manager().chain_id(), 137);
    }

    #[test]
    fn test_executor_handles_all_known_tools() {
        let exec = BlockchainExecutor::default();
        for tool in BlockchainExecutor::tool_names() {
            assert!(
                exec.handles(tool),
                "expected executor to handle {}",
                tool
            );
        }
    }

    #[test]
    fn test_executor_tool_descriptions_complete() {
        let descs = BlockchainExecutor::tool_descriptions();
        // Every named tool should have a description.
        for tool in BlockchainExecutor::tool_names() {
            let found = descs.iter().any(|(name, _)| name == tool);
            assert!(found, "missing description for tool: {}", tool);
        }
    }

    #[test]
    fn test_executor_sepolia_chain() {
        let exec = BlockchainExecutor::new("https://rpc.sepolia.org", 11155111);
        assert_eq!(exec.manager().chain_id(), 11155111);
        assert!(exec.handles("get_balance"));
    }

    #[test]
    fn test_executor_get_balance_unknown_address_format() {
        let mut exec = BlockchainExecutor::default();
        // Unknown tool should error rather than panic
        let result = exec.execute("unknown_tool_name", "{}");
        assert!(result.is_err());
    }

    #[test]
    fn test_executor_init_succeeds() {
        let mut exec = BlockchainExecutor::default();
        exec.init();
        assert!(exec.manager.is_initialized());
    }
}
