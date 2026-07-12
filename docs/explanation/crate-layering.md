# Crate layering

How the neenee workspace is split into crates, what each one owns, and the
dependency direction between them. This is the single picture to hold in your
head before reading any individual crate or ADR.

## The layer diagram

```text
neenee-core        vocabulary: events, tools, identity, principal/envoy profiles
    ^                  (no workspace deps)
    |
    +--- neenee-providers ─┐
    +--- neenee-tools      │  three peers; none depend on each other
    +--- neenee-store      ┘
    ^                  (each depends on core)
    |
neenee-agent        orchestration: Agent, the turn/round loop, tool dispatch,
    ^                  provider abstraction, skills, envoys
    |                  (depends on core + store + providers + tools)
    |
neenee-session       session runtime: SessionDriver, handlers, /btw side
    ^                  sessions, MCP runtime, serve transport, slash extension
    |                  point. Application-neutral — holds NO product name or
    |                  principal (ADR-0054).
    |                  (depends on agent + store + providers + tools + core + auth)
    |
neenee-code  ────────────────────────  application binaries. Each supplies its
(neenee-quant-bin, future)            own identity + PrincipalProfile + tools +
    |                                  custom slash commands, then drives a
    |                                  SessionDriver via neenee-session.
    +--- neenee-tui / neenee-tui-view     rendering (neenee-code's frontend)
```

The strict-DAG property from ADR-0005 is preserved: dependencies only point
upward in this diagram, never down or sideways between peers. `neenee-session`
adds exactly one node between `agent` and the applications, with zero reverse
edges (ADR-0037).

## Per-layer responsibility

### `neenee-core` — vocabulary

Pure domain types with no agent/server/TUI dependencies: `AgentRequest` /
`AgentResponse` / `Message`, the `Tool` trait, `ToolSet`, `AgentIdentity`,
`PrincipalProfile` / `EnvoyProfile`, `OperationScope`, `TokenSourceLedger`.
Everything above imports vocabulary from here. This is where role taxonomy
lives (ADR-0042), so principal and envoy profiles are declared together.

### Foundation peers — `neenee-providers`, `neenee-tools`, `neenee-store`

Three sibling crates that all depend on `core` and on nothing else in the
workspace:

- **`neenee-providers`** — concrete LLM provider impls (OpenAI, Anthropic,
  Google, xAI…) behind the `Provider` trait.
- **`neenee-tools`** — built-in tools (`bash`, `read_text`, `grep`, `glob`,
  `webfetch`, …) that self-register via `inventory`. Also the markdown
  custom-command template mechanism.
- **`neenee-store`** — durable state: `SessionStore`, `Config`, embedding index,
  repeat store, XDG paths (ADR-0014).

### `neenee-agent` — orchestration

The engine. `Agent` + the turn/round loop (ADR-0047), tool-call dispatch,
streaming, `ProxyProvider`, the skills registry, `EnvoyTool` and the
full-duplex envoy registry (ADR-0029). This crate knows how to run *one* LLM
turn with tools; it does not know about sessions, slash commands, or frontends.
Identity-agnostic and role-agnostic by design.

### `neenee-session` — session harness

The layer that turns "an engine that can run a turn" into "a running agent
session a frontend can drive." It owns:

- **`SessionDriver`** — owns one session's request receiver and long-lived
  state, then routes each `AgentRequest` to a handler until the channel closes.
- **Handlers** — `handlers_chat` / `handlers_permission` / `handlers_provider`
  / `handlers_session` / `handlers_slash`: one per `AgentRequest` group.
- **`/btw` side sessions** (`side`), **MCP runtime** (`mcp_runtime` +
  `mcp_catalog`), **pursuits**, **hooks**, **export**, **review**, **shell**.
- **`serve`** — the hot-attach WebSocket transport (ADR-0037 §7).
- **`slash_handler`** — the `SlashCommandHandler` extension point so embeddings
  register Rust slash commands without forking the server (ADR-0054).
- **`UiBridge`** — the one frontend-capability trait (`/export` clipboard).

It depends on `agent` (downward) but **never on an application** (upward).
Frontends depend on it, never the reverse.

**Application-neutral.** Per ADR-0054, this crate holds no product name,
mission, or `PrincipalProfile`. The embedding supplies an `AgentIdentity` to
`Agent::new` and binds a `PrincipalProfile` via `apply_principal_profile`.
`/btw` side sessions reuse the primary agent's identity via `Agent::identity()`.

**What's not done.** `SessionRegistry::create_session` / `close_session` are
stubs (ADR-0037 Step 6, Pending). Today there is exactly one session per
process, driven by `SessionDriver` constructed in the application's `main.rs`.
The multi-session daemon is the remaining migration step.

### Application layer — `neenee-code` (and future `neenee-quant-bin`)

The binary. `neenee-code`:

1. Constructs the `Agent` (using `neenee-agent` APIs directly — assembling the
   provider, toolset, skills, identity).
2. Binds its principal (`apply_principal_profile(&principal_code())`) — the
   identity + principal live in `neenee-code/src/identity.rs`, **not** in the
   server (ADR-0054).
3. Builds a `SessionDriver` and spawns its `run` method.
4. Runs the TUI in the main thread, holding `req_tx` / `resp_rx`.

> **Note on the current `neenee-code` dependency shape.** `neenee-code` depends
> on `neenee-agent`, `neenee-store`, `neenee-tools`, and `neenee-providers`
> *directly*, not only on `neenee-session`. This is because `SessionDriver`
> assembly (provider/toolset/agent construction) still lives in `main.rs` rather than
> behind a server-layer factory. Once `SessionRegistry::create_session` is
> populated (ADR-0037 Step 6), that assembly moves into the server and
> `neenee-code` can depend on `neenee-session` alone for orchestration. The
> direct deps are an interim "reach-through," not a design intent — see
> ADR-0037 §1 for the target DAG.

`neenee-quant` is currently a library of quant-domain tools (implements
`neenee_core::Tool`); a future `neenee-quant-bin` would mirror `neenee-code`:
bring its own quant identity + principal + tools + `/backtest`-class slash
commands, then drive the same neutral `neenee-session`.

## How a request flows across the layers

```
TUI keystroke / WS client
        │  AgentRequest (over mpsc, no source metadata)
        ▼
neenee-session: SessionDriver  ──►  handlers_*  ──►  neenee-agent: Agent::turn
        │                                                  │
        │  AgentResponse (over mpsc → TUI; cloned → broadcast → WS)  ◄──┘
        ▼
TUI renders + WS clients receive
```

The crucial property: `AgentRequest` carries **no source/client field**, so
`SessionDriver` cannot tell whether a request came from the TUI, a browser, the
`/repeat` scheduler, or an internal command tool. All frontends are
indistinguishable to the dispatcher — which is what lets them co-drive the same
session. See the [Server WebSocket API](../reference/server-api.md) for the
multi-frontend transport details.

## References

- [ADR-0005](../adr/0005-strict-layering-and-renames.md) — the strict-DAG rule.
- [ADR-0035](../adr/0035-application-layer-split.md) — application-layer split.
- [ADR-0037](../adr/0037-server-layer.md) — the server layer.
- [ADR-0053](../adr/0053-declarative-principal-profile.md) — `PrincipalProfile`.
- [ADR-0054](../adr/0054-server-layer-followups.md) — identity relocation, serve
  security, slash extension point.
