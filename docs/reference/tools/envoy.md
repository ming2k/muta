# `envoy`

`EnvoyTool` (`crates/neenee-agent/src/envoy_tool.rs`) is the dispatch
tool that spawns a research envoy. It overrides `call_structured_with_events`
to stream envoy activity back through `EnvoyEvent`, and is `Read` with
`spawns_envoy() = true`, so every envoy profile excludes it (recursion
guard).

| Parameter | Type | Required | Notes |
|-----------|------|----------|-------|
| `description` | string | yes | Max 60 chars |
| `prompt` | string | yes | Self-contained instructions for the envoy |

Spawns an envoy that inherits the parent's provider, runs isolated in its own
context, and receives only the tools admitted by the bound profile
(`EXPLORE` by default; `crates/neenee-core/src/envoy.rs`). Its final answer
is returned to the calling agent, which stays in control of all writes and any
user questions. Communication is full-duplex
([ADR-0029](../../adr/0029-full-duplex-subagent-communication.md)): a
permission or `ask_user` request the child surfaces travels up as a
`EnvoyEvent`, and the user's reply travels back down via the registry +
`EnvoyHandle`.

This page is the parameter reference. The envoy mechanism — isolation model,
event streaming, the TUI zoom view, profiles, and full-duplex — is explained in
[Envoys](../../explanation/agent-design/envoys.md). See also
[ADR-0011](../../adr/0011-subagent-profiles.md).

## `envoy_code`

A second `EnvoyTool` instance, constructed alongside `envoy` in the
bootstrap (`crates/neenee-transport/src/bootstrap.rs`), bound to the
[`CODE`](../../explanation/agent-design/envoys.md#profiles) profile. Same
parameters as `envoy` (`description`, `prompt`), same streaming and full-duplex
plumbing, but a different role: where `envoy` is the read-only research
delegation path, `envoy_code` is the **implementation** delegation path —
the analogue of kimi-code's `coder` subagent.

The two tools share one `EnvoyRegistry` (call ids are globally unique, so a
user's reply routes to the correct live child regardless of which tool spawned
it) but register as distinct capabilities under different names, so they
coexist in the parent toolset without one shadowing the other. Because `CODE`
sets `unattended: false`, every `bash`/`edit_file`/`write_file` the envoy
emits surfaces as a `EnvoyEvent::PermissionRequest` the user approves before
it executes — identical to a top-level write. See
[ADR-0086](../../adr/0086-coding-envoy-profile.md).
