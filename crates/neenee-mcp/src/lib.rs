//! MCP connector — JSON-RPC transport, server lifecycle, tool adapters, live
//! connection state, and periodic catalog refresh.
//!
//! This is the standalone `neenee-mcp` crate of ADR-0060: it owns the stdio
//! JSON-RPC client, server handles, MCP-to-`Tool` adapters, [`McpRuntime`],
//! and [`McpCatalog`]. A session (in `neenee-runtime`) owns each runtime because
//! it controls connection lifetime, user enable/disable/reconnect actions,
//! and background refresh. The agent (`neenee-agent`) has no MCP protocol
//! dependency: discovered tools reach it through the
//! [`DynamicToolSink`](neenee_contracts::DynamicToolSink) port defined in
//! `neenee-contracts`, and the agent is both that port's implementor
//! (`DynamicToolRegistry`) and consumer — the trait object doesn't care
//! which crate the MCP impl is compiled in.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod catalog;
mod client;
mod runtime;

pub use catalog::McpCatalog;
pub use client::{
    McpLoadResult, McpServer, connect_server, load_mcp_tools, reconnect_server, refresh_mcp_tools,
};
pub use runtime::{McpRuntime, ReconfigureReport};
