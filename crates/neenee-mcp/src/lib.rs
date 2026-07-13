//! MCP connector runtime for adapting remote protocol tools to neenee tools.
//!
//! This crate owns the JSON-RPC transport, server lifecycle, tool adapters,
//! live connection state, and periodic catalog refresh. A session owns an
//! [`McpRuntime`] instance and publishes its discovered tools through a core
//! [`DynamicToolSink`](neenee_core::DynamicToolSink); the agent never sees MCP
//! protocol or transport details.

mod catalog;
mod client;
mod runtime;

pub use catalog::McpCatalog;
pub use client::{
    McpLoadResult, McpServer, connect_server, load_mcp_tools, reconnect_server, refresh_mcp_tools,
};
pub use runtime::McpRuntime;
