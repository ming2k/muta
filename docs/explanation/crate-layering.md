# Crate layering

How the neenee workspace is split into crates, what each one owns, and the
dependency direction between them. This is the single picture to hold in your
head before reading any individual crate or ADR.

## The layer diagram

```text
neenee-cli ────┐
               ├──► neenee-transport ──► neenee-agent ──► neenee-persistence
neenee-server ─┘             │                  ├──► neenee-skills ────────┤
                             │                  └──► neenee-providers      │
                             └─────────────────────────────────────────────┴──► neenee-core

neenee-cli ──► neenee-tui-engine
```

An arrow means “depends on.” The diagram shows the important responsibility
edges rather than every direct Cargo edge. Higher layers may depend on
`neenee-core` directly for contracts. Both application binaries depend on
`neenee-transport`; `neenee-cli` is also the sole consumer of the TUI
engine, and its TUI view modules (formerly the `neenee-tui-view` crate) now
live inside the binary (ADR-0079). Provider implementations build on
`neenee-llm-client`, the multi-protocol HTTP client (shared transport +
OpenAI/Anthropic/Google wire protocols); OAuth credential acquisition for the
subscription providers lives in `neenee-providers`' `oauth` module.

The graph remains acyclic, but the foundation is not a set of three symmetric
peers: the built-in tools (inside `neenee-agent`) use store-owned
project/search facilities, while provider
implementations build on the `neenee-llm-client` protocol layer. The invariant
from ADR-0005 is dependency direction, not visual symmetry.

## Per-layer responsibility

### `neenee-core` — shared contracts

Pure domain and wire contracts with no workspace dependencies:
`AgentRequest` / `AgentResponse` / `Message` / `ModelRequest`, the `Provider`
and `Tool` traits, `ToolSet`, `AgentIdentity`, principal/envoy profiles,
`OperationScope`, and token-accounting records. Independent layers import the
same vocabulary without depending on agent orchestration.

Core is not the default home for all pure code. An item enters core only when
multiple independent layers exchange it, it prevents a dependency cycle, or
it is stable serialized/domain vocabulary. Agent-owned policy stays with the
agent even when it performs no I/O (ADR-0057).

### Foundation implementations — providers, skills, store, and AI SDKs

These crates implement the contracts below orchestration:

- **`neenee-llm-client`** — the multi-protocol HTTP client. Owns the pooled
  transport (`Client`, `Endpoint`, SSE byte reassembly, retry/error
  classification) and one module per wire protocol
  (`protocol::{openai, anthropic, google}`) holding the per-vendor
  request/response semantics. Each provider embeds a shared `Client` so a
  single connection pool is reused across every turn.
- **`neenee-providers`** — the channel registry and `build_provider_for_channel`
  factory, live model-list discovery, and the OAuth2/PKCE flows for
  subscription providers. It selects *which* backend to talk to;
  `neenee-llm-client` knows *how*.
- **`neenee-skills`** — skill metadata, discovery, remote caching, registry,
  periodic refresh, and `use_skill` / `list_skills` tool adapters. The agent
  consumes the registry for model-context injection; Session also reads and
  refreshes it without reaching through Agent internals.
- **`neenee-persistence`** — durable state: `SessionStore`, `Config`, embedding index,
  repeat store, XDG paths (ADR-0014).

The MCP runtime — stdio JSON-RPC transport, server lifecycle, tool adapters,
live runtime, and catalog refresh — lives **inside `neenee-agent`** as its
`mcp` module, co-located with the `ToolManager` it feeds (merged there from a
former standalone `neenee-mcp` crate). It publishes tools through the core
`DynamicToolSink` contract.

### `neenee-agent` — orchestration

The engine. `Agent` + the round/turn loop (ADR-0047), model-request and
system-prompt policy, durable conversation-context injection, tool-call
dispatch and compatibility parsing, context projection, the **MCP runtime**
(`mcp` module), shell input policy, `ProxyProvider`, skill context injection,
`EnvoyTool`, and the full-duplex envoy registry (ADR-0029). This crate knows how
to run *one* LLM round with tools. It also owns the built-in tools
(`bash`, `read_text`, `grep`, `glob`, `webfetch`, todo management, …) in its
`tools` module: most self-register via `inventory`, and stateful todo tools
receive an agent-owned context bound in `tool_integration`. It consumes
`neenee-skills` and interacts
with static and dynamic tools through core contracts. It does not know about
sessions, slash commands, or frontends. Identity-agnostic and
role-agnostic by design.

The `agent -> skills` edge is intentional layering, not a
cycle: the skills crate does not depend on agent orchestration.
`EnvoyTool` remains in Agent because it constructs and controls agents.

### `neenee-transport` — session harness

The layer that turns "an engine that can run a turn" into "a running agent
session a frontend can drive." It owns:

- **`SessionDriver`** — owns one session's request receiver and long-lived
  state, then routes each `AgentRequest` to a handler until the channel closes.
- **Handlers** — `handlers_chat` / `handlers_permission` / `handlers_provider`
  / `handlers_session` / `handlers_slash`: one per `AgentRequest` group.
- **`/btw` side sessions** (`side`), **hooks**, **export**, **review**, **shell**.
- **`serve`** — the hot-attach WebSocket transport (ADR-0037 §7).
- **`bootstrap`** — the session-harness assembly factory (ADR-0037 Step 6,
  landed by ADR-0081) that both application binaries call; identity,
  principal profile, and `UiBridge` arrive as parameters.
- **`serve_discovery`** — the discovery record a `neenee-server` writes so
  attach clients can find it (path resolution, atomic write, removal).
- **`slash_handler`** — the `SlashCommandHandler` extension point so embeddings
  register Rust slash commands without forking the server (ADR-0054).
- **`UiBridge`** — the one frontend-capability trait (`/export` clipboard).

It depends on `agent` (downward) but **never on an application** (upward).
Frontends depend on it, never the reverse.

**Application-neutral.** Per ADR-0054, this crate holds no product name,
mission, or `PrincipalProfile`. The embedding supplies an `AgentIdentity` to
`Agent::new` and binds a `PrincipalProfile` via `apply_principal_profile`.
`/btw` side sessions reuse the primary agent's identity via `Agent::identity()`.

**One session per process.** The ADR-0037 Step 6 factory has landed as
`bootstrap::assemble`: both application binaries build their
`SessionDriver` through it. Each process still hosts exactly one session;
the multi-session registry (`SessionRegistry` / `SharedState`) remains
deferred — `neenee-server` scales by one process per session, and the
registry is the right next step only if a daemon ever needs N sessions in
one process (ADR-0081).

### Application layer — `neenee-cli` and `neenee-server`

The layer now holds two binaries, both assembled through the session
layer's `bootstrap::assemble` factory:

1. **`neenee-cli`** (package name; the command is `neenee`) — the
   interactive TUI. It binds its principal
   (`apply_principal_profile(&principal_code())`) — the identity and
   principal live in the binary, **not** in the transport (ADR-0054) — then
   runs the terminal frontend. By default it drives an in-process
   standalone session; with `neenee --attach [session-id]` it instead
   attaches as a WebSocket client to a running `neenee-server` and
   co-drives that hosted session (ADR-0081).
2. **`neenee-server`** — the headless host described below.

The session-layer factory retires the dependency "reach-through" this page
used to document: provider/toolset/agent/driver assembly now lives behind
`bootstrap::assemble`, both application binaries assemble through it, and
`neenee-cli`'s direct dependencies on tool/runtime crates were pruned
accordingly — it reaches the MCP runtime through `neenee-agent`, not a
separate crate.

#### `neenee-server` — headless session host

A thin binary that hosts one session with no terminal attached: it
assembles the session through `bootstrap::assemble`, drains the driver's
responses into a broadcast channel, and serves the session over WebSocket
with the same protocol `/serve` uses. One server hosts one session.
Clients find it through a per-project discovery record (written on
startup, removed on clean shutdown), and `neenee --attach` spawns one on
demand when none is running. See ADR-0081 for the attach model and its v1
limitations.

## How a request flows across the layers

```
TUI keystroke / WS client
        │  AgentRequest (over mpsc, no source metadata)
        ▼
neenee-transport: SessionDriver  ──►  handlers_*  ──►  neenee-agent: Agent::turn
        │                                                  │
        │  AgentResponse (over mpsc → TUI; cloned → broadcast → WS)  ◄──┘
        ▼
TUI renders + WS clients receive
```

The crucial property: `AgentRequest` carries **no source/client field**, so
`SessionDriver` cannot tell whether a request came from the TUI, a browser, the
`/schedule` (and `/repeat` cron alias) scheduler, or an internal command tool. All frontends are
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
- [ADR-0057](../adr/0057-contract-only-core-boundary.md) — contract-only core
  admission rule and agent-owned pure policy.
- [ADR-0059](../adr/0059-agent-tool-integration-boundary.md) — direct
  agent-to-tools integration and stateful tool construction.
- [ADR-0060](../adr/0060-skills-and-mcp-extension-boundaries.md) — separate
  skill/MCP capability crates and dynamic tool publication.
- [ADR-0079](../adr/0079-remerge-tui-view-into-binary.md) — the view crate's
  re-merge into the application binary.
- [ADR-0080](../adr/0080-rename-neenee-to-neenee-cli.md) — the package rename
  `neenee` → `neenee-cli` (command unchanged).
- [ADR-0081](../adr/0081-neenee-server-and-attach-model.md) — the server
  binary, the `bootstrap::assemble` factory, and the attach model.
