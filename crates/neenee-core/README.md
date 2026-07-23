# neenee-core

Shared domain and wire contracts for the neenee agent stack.

This crate is the **zero-I/O domain core** (ADR-0005): no `rusqlite`, no
filesystem, no network. It is the dependency-inversion boundary used by
independent providers, tools, persistence, sessions, SDK adapters, and
frontends:

- the [`Provider`] and [`Tool`] capability traits plus the atomic
  [`ModelRequest`] exchanged by agents and providers (in
  [`capability.rs`][cap]);
- conversation, event, and tool-output protocol types;
- shared value policy such as capability scopes and context budgets;
- pursuit / repeat / todo domain types, envoy profiles, skills/MCP config
  schemas;
- the wire events the harness and frontends exchange.

Code belongs here only when multiple layers exchange it or when the contract is
needed to prevent a dependency cycle. Pure agent behavior does not belong here
merely because it performs no I/O; orchestration policy, prompt composition,
and agent-owned runtime state live in [`neenee-agent`](../neenee-agent).

Frontends and sibling services depend on `neenee-core` for contracts and add
their own behavior or I/O above it. Persistence belongs in
[`neenee-persistence`](../neenee-persistence), provider transports in the AI SDK/provider
crates, and orchestration in `neenee-agent`.

See the architecture overview in [`docs/`](../../../docs/), ADR-0005 for the
zero-I/O dependency rule, and ADR-0057 for the contract-only admission rule.

[`Provider`]: src/capability.rs
[`Tool`]: src/capability.rs
[`ModelRequest`]: src/capability.rs
[cap]: src/capability.rs
