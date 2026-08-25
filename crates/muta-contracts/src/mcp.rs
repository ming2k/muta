//! Shared configuration schema for MCP servers.
//!
//! Lives in `muta-contracts` for the same reason `WebSearchConfig` does: both the
//! app-layer `Config` (which owns the `[mcp]` table), `muta-agent` (which owns the MCP connector), and the
//! session/frontend layers exchange these values without depending on one
//! another's implementation details.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// One MCP server entry from the `[mcp.<name>]` table of `config.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct McpServerConfig {
    pub command: Vec<String>,
    pub environment: HashMap<String, String>,
    pub enabled: bool,
    pub read_only: bool,
    /// Runtime-only origin marker. Project-defined servers carry their exact
    /// workspace root and must be launched read-only/offline inside the
    /// workspace sandbox. Global user configuration leaves this unset.
    #[serde(skip)]
    pub sandbox_root: Option<PathBuf>,
}

impl Default for McpServerConfig {
    fn default() -> Self {
        Self {
            command: Vec::new(),
            environment: HashMap::new(),
            enabled: true,
            read_only: false,
            sandbox_root: None,
        }
    }
}

/// Runtime status reported by `muta-agent` (MCP connector) for each configured server.
///
/// Lives in `muta-contracts` (alongside [`McpServerConfig`]) so the TUI can
/// consume it without depending on the connector implementation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpConnectionStatus {
    /// A connection attempt is in flight (background connect at startup, or a
    /// reconnect). The server is not usable yet; the model sees none of its
    /// tools until it transitions to `Connected`.
    Connecting,
    Connected {
        tools: usize,
    },
    Disabled,
    Failed(String),
}

impl std::fmt::Display for McpConnectionStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Connecting => write!(f, "connecting…"),
            Self::Connected { tools } => write!(f, "connected ({} tools)", tools),
            Self::Disabled => write!(f, "disabled"),
            Self::Failed(error) => write!(f, "failed: {}", error),
        }
    }
}
