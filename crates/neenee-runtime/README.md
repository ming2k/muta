# neenee-transport

The transport layer between the orchestration crate (`neenee-agent`) and the
frontends (`neenee` TUI today, a browser frontend tomorrow).

## Why this crate exists

Historically `neenee` was a single process: one TUI driving one agent
background task over a pair of `mpsc` channels. When the TUI process and the
agent process were split apart, this crate became the bridge — it owns the
long-lived agent session, multiplexes requests/responses, and exposes a stable
transport so any frontend can attach.

It depends on `neenee-agent` for the orchestration loop and on
`neenee-persistence` for durable state; frontends depend only on the transport
surface exposed here.

## Application-neutral

This crate holds **no product name, mission, or principal profile** (ADR-0054).
The embedding binary supplies an `AgentIdentity` to `Agent::new` and binds a
`PrincipalProfile` via `apply_principal_profile`. `neenee` keeps the coding
identity; a future sibling binary would bring its own. The `/btw` side session
reuses the primary agent's identity via `Agent::identity()` rather than naming a
product here.

## What it provides

- **`session_driver`** — `SessionDriver` owns one session's request loop and
  routes `AgentRequest`s to handlers.
- **Handlers** — chat, permission, provider, session, slash (the 23 built-in
  commands).
- **`slash_handler`** — a `SlashCommandHandler` trait + `SlashCommandRegistry`
  so embeddings register Rust slash commands without forking this crate
  (ADR-0054).
- **`serve`** — the hot-attach WebSocket transport. Loopback by default;
  `--public` binds all interfaces and requires a bearer token (ADR-0054).
- **`/btw` side sessions**, **MCP runtime**, **hooks**, **export**,
  **review**, **shell**.
- **`UiBridge`** — the one frontend-capability trait (`/export` clipboard).

## Frontend protocol

The current hot-attach WebSocket API is documented in the
[frontend integration guide](../../docs/reference/server-api.md) and the
[machine-readable AsyncAPI contract](../../docs/reference/server.asyncapi.yaml).
The crate layering is described in
[crate layering](../../docs/explanation/crate-layering.md). See ADR-0037 and
ADR-0054 for the design decisions.

## Status

`SessionRegistry::create_session` / `close_session` are stubs (ADR-0037 Step 6,
Pending). Today both application binaries build one `SessionDriver` per process
through the `bootstrap::assemble` factory (ADR-0081); moving that assembly into
the registry is the remaining daemon step.
