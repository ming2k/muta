# MCP tools

Each MCP server's tools are wrapped in `McpTool`
(`crates/neenee-mcp/src/client.rs`) and dispatch `tools/call` JSON-RPC over
stdio to the server child process. The wrapper inherits the server's
`read_only` flag as its `ToolAccess`: a `read_only` server's tools are `Read`,
and any other server's are `Write`. This classification affects permission
policy; it does not automatically propagate the connection to an envoy or
side agent. Connect and `tools/list` are bounded by
`MCP_CONNECT_TIMEOUT = 8s`.
Configuration lives in `config.toml` under `[mcp.<server>]`.

## `mcp__<server>__<tool>`

Parameters come from the MCP server's `inputSchema`, falling back to
`{"type":"object"}` when absent (`crates/neenee-mcp/src/client.rs`). The public
name is `mcp__{sanitized_server}__{sanitized_original}`.

See [MCP servers](../../explanation/agent-design/mcp.md) for the server model,
quarantine behaviour, lifecycle ownership, and delegation boundary.
