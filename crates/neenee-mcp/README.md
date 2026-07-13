# neenee-mcp

The Model Context Protocol connector: stdio JSON-RPC transport, server
processes, MCP-to-`Tool` adapters, live connection state, and catalog refresh.

A session owns `McpRuntime` and publishes complete per-server tool snapshots
through `neenee_core::DynamicToolSink`. The agent consumes those tools without
depending on MCP protocol or transport details. The application chooses the
sink explicitly; neenee-code attaches its runtime to the principal agent and
does not implicitly propagate external connections to temporary agents.
