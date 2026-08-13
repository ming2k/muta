//! MCP connector — JSON-RPC transport, server lifecycle, tool adapters, live
//! connection state, and periodic catalog refresh.
//!
//! Formerly the standalone `neenee-mcp` crate, now co-located with the agent
//! (matching kimi-code's layout, where MCP clients and the ToolManager live in
//! the same `agent-core` package). The agent owns a [`crate::mcp::McpRuntime`] and publishes
//! discovered tools through a core [`DynamicToolSink`](neenee_core::DynamicToolSink);
//! the rest of the agent never sees MCP protocol or transport details.
//!
//! Co-location doesn't weaken the dependency inversion: the
//! `DynamicToolSink` trait still lives in `neenee-core`, and the agent is both
//! its implementor (`DynamicToolRegistry`) and consumer — the trait object
//! doesn't care which crate the impl is compiled in.

mod catalog;
mod client;
mod runtime;

pub use catalog::McpCatalog;
pub use client::{
    McpLoadResult, McpServer, connect_server, load_mcp_tools, reconnect_server, refresh_mcp_tools,
};
pub use runtime::{McpRuntime, ReconfigureReport};
