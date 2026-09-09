# `spawn_agent` / `delegate_code` / `delegate_mcp`

`SubagentTool` (`crates/muta-agent/src/subagent_tool.rs`) is the dispatch tool
that spawns a focused subagent. It overrides `call_structured_with_events` to
stream subagent activity back through `SubagentEvent`, and is `Read` with
`spawns_subagent() = true`, so every subagent preset excludes it (recursion
guard).

| Parameter | Type | Required | Notes |
|-----------|------|----------|-------|
| `description` | string | yes | Max 60 chars |
| `prompt` | string | yes | Self-contained instructions for the subagent |
| `role` | string | no | `"explore"` (default), `"code"`, `"mcp"`, or `"skill"` |
| `background` | bool | no | Dispatch asynchronously; read-only roles only |

Spawns a subagent that inherits the parent's provider, runs isolated in its own
context, and receives only the tools admitted by the bound preset
(`SUBAGENT_EXPLORE` by default; `crates/muta-contracts/src/subagent.rs`). Its
final answer is returned to the calling agent, which stays in control of
top-level writes and user interactions. Communication is full-duplex
([ADR-0029](../../adr/0029-full-duplex-subagent-communication.md)): a
permission or `ask_user` request the child surfaces travels up as a
`SubagentEvent`, and the user's reply travels back down via the registry +
`SubagentHandle`.

This page is the parameter reference. The subagent mechanism — isolation model,
event streaming, the TUI zoom view, presets, and full-duplex — is explained in
[Subagents](../../explanation/agent-design/subagents.md). See also
[ADR-0144](../../adr/0144-three-tier-agent-hierarchy-and-tool-pool.md).

## `delegate_code` and `delegate_mcp`

Two more `SubagentTool` instances bind other presets to their own tool names:
[`SUBAGENT_CODE`](../../explanation/agent-design/subagents.md#profiles) as
`delegate_code` (the implementation delegation path, with the same parameters
as `spawn_agent`), and `SUBAGENT_MCP_SPECIALIST` as `delegate_mcp` (specialized
integration work with dynamic/MCP tools in an isolated sandbox).

The tools share one `SubagentRegistry` (call ids are globally unique, so a
user's reply routes to the correct live child regardless of which tool spawned
it) but register as distinct capabilities under different names, so they
coexist in the parent toolset without one shadowing the other. `SUBAGENT_CODE`
runs `delegated: true` like other subagents — the principal's act of calling
`delegate_code` is the authorization for the delegated task, so the child's
writes and commands execute on the subagent's own authority and do not route
through the permission broker. (`ask_user` still uses the full-duplex channel.)
See [ADR-0087](../../adr/0087-code-envoy-runs-autopilot.md) (supersedes
ADR-0086's attended default).
