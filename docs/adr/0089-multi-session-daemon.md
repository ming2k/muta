# 0089. Multi-session daemon (`SessionRegistry` + select-then-attach protocol)

- **Status:** Accepted
- **Date:** 2026-07-30
- **Builds on:** ADR-0081 (server-and-attach model), ADR-0018
  (per-project multi-instance concurrency)
- **Revises:** ADR-0081's "one server hosts one session" v1 simplification

## Context

ADR-0081 shipped attach mode with a deliberate v1 simplification: **one
`neenee-server` process hosts exactly one session**, and the discovery record
is per-project-bucket (last-write-wins). That left two rough edges the ADR
itself flagged as deferred:

- *"Multi-session server (revive `SessionRegistry`). Deferred again: one
  server hosts one session; multiple sessions are multiple spawned servers."*
- `neenee attach <id>` required the user to know a session id up front, with
  no picker — because there was nothing to pick among (the server hosted one).

In practice a user runs many sessions in a project. Wanting a single
long-lived *host* that owns several sessions — the tmux/docker mental model
the word "server" promises — is the natural expectation. Spawning a whole
process per session also multiplies startup cost (config discovery, MCP
connects, skill scans) that a shared host would pay once.

## Decision

### 1. `SessionRegistry`: one process, many sessions

A new `neenee_transport::registry::SessionRegistry` owns the live sessions a
host process is serving: a `HashMap<SessionId, HostedSession>`, where each
`HostedSession` is an independent harness from `bootstrap::assemble`. The
bootstrap factory is per-session and reentrant (`SessionStore` is an
instance-per-path value with no global lock — side conversations `/btw`
already run two stores in one process); `SessionDriver::run(self)` owns one
session and shares no mutables; the opt-in per-project `ProcessLock` is off
by default. The ADR-0018 one-writer invariant is preserved **by
construction** — each hosted session still pins its own
`sessions/<id>.{json,jsonl}` and is that file's only writer; the registry
merely multiplexes several in one process.

Lazy resume: a session id not currently hosted but present on disk is
assembled on demand when a client selects it.

### 2. Select-then-attach handshake (replaces immediate `History`)

`Wire::History` is retired. The protocol gains a short negotiation phase
right after the WS upgrade:

```rust
enum Wire {
    Select { action: AttachAction },            // client -> server, first frame
    Welcome { session_id, round_counter, messages }, // server -> client
    Pick { sessions: Vec<SessionOverview> },    // server -> client (choose)
    Error { message },                          // server -> client, terminal
    Request { request: AgentRequest },          // streaming, unchanged
    Response { response: AgentResponse },       // streaming, unchanged
}
enum AttachAction { New, Attach(Option<String>) }
```

Routing: `New` -> fresh session; `Attach(None)` -> the single live session,
or `Pick` if several; `Attach(Some(id))` -> that hosted session, or a lazy
resume from disk, or `Error` if unknown. `Pick` carries the merged live +
on-disk set as `SessionOverview` — the exact type the existing TUI picker
renders.

This is a breaking change to the attach wire format. It is acceptable: the
entire attach model (ADR-0081) is unreleased, server and client ship
together, and no compatibility shim is warranted.

### 3. `/serve` unified onto the registry

The in-TUI `/serve` command now constructs a one-entry prehost registry for
the live TUI session and speaks the same protocol. One code path serves
both.

### 4. CLI surface

- `neenee daemon` — start the host in the foreground (= `neenee-server`;
  the binary is the implementation, the subcommand is the verb).
- `neenee attach` (no id) — `Attach(None)`; if the daemon answers `Pick`,
  the sessions are listed (the in-TUI picker integration is a follow-up).
- `neenee attach <id>` — `Attach(Some(id))` directly.

The standalone path (`neenee`, `neenee resume`, `neenee doctor`) is
unchanged and remains the default.

### 5. Discovery record becomes daemon-scoped

`serve_discovery::Discovery` drops its `session_id` field: a daemon is not
tied to one session. The record keeps `pid`, `port`, `token`,
`project_root`, `started_at`; clients resolve the daemon by project bucket
and negotiate the session over the socket.

## Alternatives considered

- **Keep one-server-per-session, spawn N processes.** Rejected: multiplies
  per-process startup cost N times; provides no shared state for future
  cross-session features; reads as "not a real daemon." The only real
  blocker to in-process multiplexing was the per-project discovery file,
  which this ADR removes.
- **Don't change the protocol; have the daemon pre-pick a session.**
  Rejected: the client would have no way to choose among hosted sessions.
- **Full process-supervisor daemon (auto-restart, sockets, logging).** Out
  of scope: this ADR makes one process host N sessions; it does not turn
  neenee into a service manager.

## Consequences

- **Positive.** One host process serves a whole project's sessions; startup
  cost is paid once; the wire protocol honestly models "a host with
  sessions" instead of "a session with a side channel."
- **Positive (correctness).** ADR-0018's one-writer invariant is untouched —
  distinct sessions are distinct files with distinct writers.
- **Breaking.** The attach wire format changes; `neenee-server --session`
  is removed; the discovery record loses `session_id`. All unreleased;
  recorded in the changelog.

## References

- [ADR-0081](0081-neenee-server-and-attach-model.md) — the single-session
  host this revises.
- [ADR-0018](0018-per-project-multi-instance-concurrency.md) — the
  one-writer invariant the registry preserves by construction.
- [ADR-0088](0088-attach-subcommand.md) — `--attach` -> `attach`.
