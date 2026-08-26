# MCP tools

Each MCP server's tools are wrapped in `McpTool`
(`crates/muta-mcp/src/client.rs`) and dispatch `tools/call` JSON-RPC to the
server over its configured transport — a spawned stdio child (`command`) or a
Streamable HTTP endpoint (`url`). The wrapper inherits the server's
`read_only` flag as its `ToolAccess`: a `read_only` server's tools are `Read`,
and any other server's are `Write`. This classification affects permission
policy; it does not automatically propagate the connection to an envoy or
side agent. Connect and `tools/list` are bounded by
`MCP_CONNECT_TIMEOUT = 8s`.
Configuration lives in `config.toml` under `[mcp.<server>]`.

## Transports

`[mcp.<server>]` declares exactly one transport; `url` wins when both are
present:

- `command = ["uvx", "mcp-server-fetch"]` — a local stdio child process.
- `url = "https://example.com/mcp"` — a Streamable HTTP endpoint. Responses
  may arrive as plain JSON or SSE-framed `data:` lines; both are parsed.
  A server-issued `Mcp-Session-Id` is captured at initialize and echoed on
  every subsequent request. HTTP-level failures (connection refused, TLS,
  HTTP/2 reset) classify as transport errors and reconnect-retry once;
  delivered 4xx/5xx bodies classify as protocol errors and never retry.

The initialize handshake offers the client's newest protocol revision
(`2025-06-18`) and accepts any server answer in the supported set
(`2025-06-18`, `2025-03-26`, `2024-11-05`); an unsupported revision fails the
connection.

## Config-time tool scoping

`[mcp.<server>]` accepts two optional lists matched against the server's
*original* (unsanitized) tool names:

- `allow_tools = ["fetch", "search"]` — when non-empty, only listed tools are
  published.
- `deny_tools = ["dangerous_tool"]` — never published; deny wins over allow.

Filtered tools are never adapted, so the model never sees them. The same
fields exist in project-local `.muta/mcp.json` server entries.

## `mcp__<server>__<tool>`

Parameters come from the MCP server's `inputSchema`, falling back to
`{"type":"object"}` when absent (`crates/muta-mcp/src/client.rs`). The public
name is `mcp__{sanitized_server}__{sanitized_original}`.

See [MCP servers](../../explanation/agent-design/mcp.md) for the server model,
quarantine behaviour, lifecycle ownership, and delegation boundary.
