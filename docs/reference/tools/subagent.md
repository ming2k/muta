# `spawn_agent` / `delegate_code`

`SubagentTool` (`crates/muta-agent/src/subagent_tool.rs`) is the dispatch tool
that spawns a focused subagent. It overrides `call_structured_with_events` to
stream subagent activity back through `SubagentEvent`, and is `Read` with
`spawns_subagent() = true`, so every subagent preset excludes it (recursion
guard).

| Parameter | Type | Required | Notes |
|-----------|------|----------|-------|
| `description` | string | yes | Max 60 chars |
| `prompt` | string | yes | Self-contained instructions for the subagent |
| `role` | string | no | `"explore"` (default), `"code"`, or `"skill"` |

The call returns when the child finishes: it runs inside the calling turn, so
`delegate_code` and its siblings cannot be dispatched to the background job
fabric. The former `background` parameter is gone from the schema
([ADR-0234](../../adr/0234-authorized-background-job-continuations.md)) because
it promised an asynchronous dispatch the implementation never performed — no
job registration, no `job_id`, no result to collect. Passing it explicitly is
rejected with an actionable error rather than silently running synchronously.
Background *shell* work is available through `run_command`'s `background` and
`service` modes.

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

## Roles

`role` selects the child's **capability grant and persona**, not a separate
tool. The enum in the schema and the runtime check come from one constant
(`DISPATCH_ROLES` in `crates/muta-agent/src/subagent_tool.rs`), so a role the
schema offers always resolves and a role it does not offer is refused with the
dispatchable set named — it is never silently downgraded to the bound default.

| Role | Grant | Notes |
|------|-------|-------|
| `explore` | read-only inspection | The default. Read-only, non-interactive, non-recursive. |
| `code` | read-only + write/execute | Delegated implementation work; the delegation is the authorization (ADR-0087). |
| `skill` | read-only inspection | Persona framing for skill discovery. Shares `explore`'s toolset exactly. |

`title` is deliberately **not** dispatchable: it is a harness-internal role
(session titling drives it directly) and must not become spawnable just because
it lives in the same preset pool.

There is no separate `delegate_code` tool in the shipped
binary. `SubagentTool::named` can construct such instances — the type supports
it, and the TUI renders those names — but only `spawn_agent` is registered, and
its `role` enum is how a caller reaches the other grants. A `role` therefore
costs no extra tool schema. Under ADR-0240, MCP tools are dedicated exclusively
to the Master Agent, so the retired `mcp` subagent role and `delegate_mcp` have been removed.

Each child receives only the tools its preset admits
(`crates/muta-contracts/src/subagent.rs`), regardless of role. A role can only
*narrow* the parent's authority: recursion (`spawn_agent`) and control-flow
tools are excluded absolutely, and the master's delegation policy must admit the
preset at all.
