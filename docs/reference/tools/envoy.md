# `runner` / `spawn_runner`

`RunnerTool` (`crates/muta-agent/src/runner_tool.rs`) is the dispatch
tool that spawns a focused runner. It overrides `call_structured_with_events`
to stream runner activity back through `RunnerEvent`, and is `Read` with
`spawns_runner() = true`, so every runner profile excludes it (recursion
guard).

| Parameter | Type | Required | Notes |
|-----------|------|----------|-------|
| `description` | string | yes | Max 60 chars |
| `prompt` | string | yes | Self-contained instructions for the runner |
| `role` | string | no | Role/preset: `"explore"` (default), `"code"`, or `"mcp"` |

Spawns a runner that inherits the parent's provider, runs isolated in its own
context, and receives only the tools admitted by the bound profile
(`RUNNER_EXPLORE` by default; `crates/muta-contracts/src/runner.rs`). Its final answer
is returned to the calling agent, which stays in control of top-level writes and
user interactions. Communication is full-duplex
([ADR-0029](../../adr/0029-full-duplex-subagent-communication.md)): a
permission or `ask_user` request the child surfaces travels up as a
`RunnerEvent`, and the user's reply travels back down via the registry +
`RunnerHandle`.

This page is the parameter reference. The runner mechanism — isolation model,
event streaming, the TUI zoom view, profiles, and full-duplex — is explained in
[Runners](../../explanation/agent-design/envoys.md). See also
[ADR-0144](../../adr/0144-three-tier-agent-hierarchy-and-tool-pool.md).

## `runner_code`

A specialized `RunnerTool` instance bound to the
[`RUNNER_CODE`](../../explanation/agent-design/envoys.md#profiles) profile. Same
parameters as `spawn_runner` (`description`, `prompt`), same streaming and full-duplex
plumbing, but a different role: where `runner` is the read-only research
delegation path, `runner_code` is the **implementation** delegation path.

The tools share one `RunnerRegistry` (call ids are globally unique, so a
user's reply routes to the correct live child regardless of which tool spawned
it) but register as distinct capabilities under different names, so they
coexist in the parent toolset without one shadowing the other. `RUNNER_CODE` runs
`delegated: true` like other runners — the principal's act of
calling `runner_code` is the authorization for the delegated task, so the
child's writes and commands execute on the runner's own authority and do not
route through the permission broker. (`ask_user` still uses the full-duplex
channel.) See
[ADR-0087](../../adr/0087-code-envoy-runs-autopilot.md)
(supersedes ADR-0086's attended default).
