# neenee-agent

The orchestration layer between the pure domain (`neenee-core`) and the
application services (`neenee-persistence`) on one side, and the frontends on the
other.

## What lives here

- **The `Agent` struct** (`agent.rs`) — holds the provider, tool set, mode,
  and optional skill registry; runs the streaming ReAct loop.
- **The round/turn loop** — tool-call parsing, permission brokering, context
  pressure (summarisation / context projection per ADR-0029), and the steering
  inbox.
- **Conversation and model-request policy** — durable lifecycle-driven context
  additions, ephemeral system-prompt/tool snapshot assembly, context
  projection, and compatibility parsing for text-emitted
  tool calls.
- **Agent/tool integration** — construction of concrete tools bound to
  agent-owned state, custom-tool extension through `AgentBuilder`, and shell
  input policy. The built-in tools, including the todo implementations and
  their context, live in this crate's `tools` module; the agent binds them
  automatically.
- **Extension integration** — optional `neenee-skills` context injection, a
  connector-neutral dynamic-tool sink, and the **MCP runtime** (`mcp` module:
  stdio JSON-RPC transport, server lifecycle, tool adapters, live runtime,
  catalog refresh).
- **Catalog & envoy** — model/channel resolution and sub-agent ("envoy")
  profiles.

This crate owns behavior even when that behavior is implemented as pure code.
Only contracts shared with independent layers stay in `neenee-core` (ADR-0057),
including the atomic `ModelRequest` exchanged with providers (ADR-0061).
The agent drives `neenee-persistence` and `neenee-providers` and consumes
`neenee-skills` through normal downward
dependencies. The turn loop still dispatches through core tool contracts.
Frontends (`neenee`) sit above it via `neenee-transport`.
