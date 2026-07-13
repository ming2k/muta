# neenee-agent

The orchestration layer between the pure domain (`neenee-core`) and the
application services (`neenee-store`) on one side, and the frontends on the
other.

## What lives here

- **The `Agent` struct** (`agent.rs`) — holds the provider, tool set, mode,
  pursuit, and optional skill registry; runs the streaming ReAct loop.
- **The turn/round loop** — tool-call parsing, permission brokering, context
  pressure (summarisation / context projection per ADR-0029), and the steering
  inbox.
- **Model-context policy** — system-prompt composition, context projection,
  pursuit continuation, and compatibility parsing for text-emitted tool calls.
- **Agent/tool integration** — construction of concrete tools bound to
  agent-owned state, custom-tool extension through `AgentBuilder`, and shell
  input policy. Todo implementations and their context live in
  `neenee-tools`; the agent binds them automatically.
- **Extension integration** — optional `neenee-skills` context injection and a
  connector-neutral dynamic-tool sink. MCP protocol/runtime lives outside the
  agent in `neenee-mcp`.
- **Catalog & envoy** — model/channel resolution and sub-agent ("envoy")
  profiles.

This crate owns behavior even when that behavior is implemented as pure code.
Only contracts shared with independent layers stay in `neenee-core` (ADR-0057).
The agent drives `neenee-store` and `neenee-providers` and consumes the
concrete `neenee-tools` bundle and `neenee-skills` through normal downward
dependencies. The turn loop still dispatches through core tool contracts.
Frontends
(`neenee-code`, `neenee-quant`) sit above it via `neenee-session`'s transport.
