# Crate layering

How the neenee workspace is split into crates, what each one owns, and the
dependency direction between them. This is the single picture to hold in your
head before reading any individual crate or ADR.

## The layer diagram

```text
neenee ──► neenee-transport ──► neenee-agent ──► neenee-tools ──► neenee-persistence
                     │                  ├──► neenee-skills ────────────────┤
                     │                  └──► neenee-providers              │
                     ├──► neenee-mcp ─────────────────────────────────────┤
                     └─────────────────────────────────────────────────────┴──► neenee-core

neenee ──► neenee-tui-view ──► neenee-tui-engine
```

An arrow means “depends on.” The diagram shows the important responsibility
edges rather than every direct Cargo edge. Higher layers may depend on
`neenee-core` directly for contracts. Provider implementations build on
`neenee-llm-client`, the multi-protocol HTTP client (shared transport +
OpenAI/Anthropic/Google wire protocols); authentication is another downward
dependency of `neenee-agent`.

The graph remains acyclic, but the foundation is not a set of three symmetric
peers: tools use store-owned project/search facilities, while provider
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

### Foundation implementations — providers, tools, skills, MCP, store, and AI SDKs

These crates implement the contracts below orchestration:

- **`neenee-llm-client`** — the multi-protocol HTTP client. Owns the pooled
  transport (`Client`, `Endpoint`, SSE byte reassembly, retry/error
  classification) and one module per wire protocol
  (`protocol::{openai, anthropic, google}`) holding the per-vendor
  request/response semantics. Each provider embeds a shared `Client` so a
  single connection pool is reused across every turn.
- **`neenee-providers`** — the channel registry and `build_provider_for_channel`
  factory, plus live model-list discovery and the in-memory mock provider. It
  selects *which* backend to talk to; `neenee-llm-client` knows *how*.
- **`neenee-tools`** — built-in tools (`bash`, `read_text`, `grep`, `glob`,
  `webfetch`, todo management, …). Most self-register via `inventory`;
  stateful todo tools receive an agent-owned context from
  `neenee-agent::tool_integration`. Store-backed tools and project helpers
  depend on `neenee-persistence`.
- **`neenee-skills`** — skill metadata, discovery, remote caching, registry,
  periodic refresh, and `use_skill` / `list_skills` tool adapters. The agent
  consumes the registry for model-context injection; Session also reads and
  refreshes it without reaching through Agent internals.
- **`neenee-mcp`** — stdio JSON-RPC transport, MCP server lifecycle, tool
  adapters, live runtime, and catalog refresh. It publishes tools through the
  core `DynamicToolSink` contract and has no dependency on Agent or Session.
- **`neenee-persistence`** — durable state: `SessionStore`, `Config`, embedding index,
  repeat store, XDG paths (ADR-0014).

### `neenee-agent` — orchestration

The engine. `Agent` + the turn/round loop (ADR-0047), model-request and
system-prompt policy, durable conversation-context injection, tool-call
dispatch and compatibility parsing, context projection, pursuit continuation,
shell input policy, `ProxyProvider`, skill context injection,
`EnvoyTool`, and the full-duplex envoy registry (ADR-0029). This crate knows how
to run *one* LLM round with tools. It directly consumes `neenee-tools` and
`neenee-skills`, binds agent-owned state to concrete todo tools, and interacts
with static and dynamic tools through core contracts. It does not know about
MCP protocol, sessions, slash commands, or frontends. Identity-agnostic and
role-agnostic by design.

The `agent -> tools` and `agent -> skills` edges are intentional layering, not
cycles: neither implementation crate depends on agent orchestration.
`EnvoyTool` remains in Agent because it constructs and controls agents.

### `neenee-transport` — session harness

The layer that turns "an engine that can run a turn" into "a running agent
session a frontend can drive." It owns:

- **`SessionDriver`** — owns one session's request receiver and long-lived
  state, then routes each `AgentRequest` to a handler until the channel closes.
- **Handlers** — `handlers_chat` / `handlers_permission` / `handlers_provider`
  / `handlers_session` / `handlers_slash`: one per `AgentRequest` group.
- **`/btw` side sessions** (`side`), ownership of the **`neenee-mcp`
  runtime**, **pursuits**, **hooks**, **export**, **review**, **shell**.
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

**What's not done.** Today there is exactly one session per process, driven by
`SessionDriver` constructed in the application's `main.rs`. The multi-session
daemon (ADR-0037 Step 6) remains a future migration step; its dormant
`SessionRegistry` / `SharedState` scaffolding was removed because every method
returned `Err("not yet populated")` — reintroduce it when the server move
resumes.

### Application layer — `neenee`

The binary. `neenee`:

1. Constructs the `Agent` (using `neenee-agent` APIs directly — supplying the
   provider, configured toolset, identity), then attaches a live skill registry
   when that application enables skills. The agent adds tools tied to its own
   runtime state.
2. Binds its principal (`apply_principal_profile(&principal_code())`) — the
   identity + principal live in `neenee/src/identity.rs`, **not** in the
   server (ADR-0054).
3. Builds a `SessionDriver` and spawns its `run` method.
4. Runs the TUI in the main thread, holding `req_tx` / `resp_rx`.

> **Note on the current `neenee` dependency shape.** `neenee` depends
> on `neenee-agent`, `neenee-persistence`, `neenee-tools`, `neenee-skills`,
> `neenee-mcp`, and `neenee-providers`
> *directly*, not only on `neenee-transport`. This is because `SessionDriver`
> assembly (provider/configured-toolset/agent construction) still lives in
> `main.rs` rather than behind a session-layer factory. If a session-layer
> factory is reintroduced (ADR-0037 Step 6; the first dormant scaffolding was
> removed), that assembly moves into the session layer and `neenee` can
> depend on `neenee-transport` alone for orchestration. The direct deps are an
> interim “reach-through,” not a design intent — see ADR-0037 §1 for the
> target DAG.

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
- [ADR-0057](../adr/0057-contract-only-core-boundary.md) — contract-only core
  admission rule and agent-owned pure policy.
- [ADR-0059](../adr/0059-agent-tool-integration-boundary.md) — direct
  agent-to-tools integration and stateful tool construction.
- [ADR-0060](../adr/0060-skills-and-mcp-extension-boundaries.md) — separate
  skill/MCP capability crates and dynamic tool publication.
