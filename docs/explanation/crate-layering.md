# Crate layering

How the Muta workspace is split into crates, what each one owns, and the
dependency direction between them. This is the single picture to hold in your
head before reading any individual crate or ADR.

## The layer diagram

```text
muta core ──┐
            ├──► muta-runtime ──► muta-agent ──► muta-persistence ──┐
mutx app ───┘          │   │            ├──► muta-skills ──────────┤
     │                 │   │            └──► muta-providers ───────┼──► muta-contracts
     │                 │   └──► muta-mcp ──────────────────────────┤          ▲
     │                 └───────────────────────────────────────────┤          │
     └──► mutx-engine ─────────────────────────────────────────────┴──► muta-platform

web app ── control protocol ──► muta-runtime
```

An arrow means “depends on.” The diagram shows the important responsibility
edges rather than every direct Cargo edge. Higher layers may depend on
`muta-contracts` directly for contracts. The `muta` core and `mutx` terminal
app are separate binaries that both depend on `muta-runtime`; only `mutx`
depends on the TUI and its engine. Provider implementations build on
`muta-llm-client`, the multi-protocol HTTP client (shared transport +
OpenAI/Anthropic/Google wire protocols); OAuth credential acquisition for the
subscription providers lives in `muta-providers`' `oauth` module.

The graph remains acyclic, but the foundation is not a set of three symmetric
peers: the built-in tools (inside `muta-agent`) use store-owned
project/search facilities, while provider
implementations build on the `muta-llm-client` protocol layer. The invariant
from ADR-0005 is dependency direction, not visual symmetry.

## Per-layer responsibility

### `muta-contracts` — shared contracts

Pure domain and wire contracts with no workspace dependencies:
`AgentRequest` / `AgentResponse` / `Message` / `ModelRequest`, the `Provider`
and `Tool` traits, `ToolSet`, `AgentIdentity`, principal/envoy profiles,
`OperationScope`, and token-accounting records. Independent layers import the
same vocabulary without depending on agent orchestration.

Contracts is not the default home for all pure code. An item enters
`muta-contracts` only when
multiple independent layers exchange it, it prevents a dependency cycle, or
it is stable serialized/domain vocabulary. Agent-owned policy stays with the
agent even when it performs no I/O (ADR-0057).

### Foundation implementations — providers, persistence, and the LLM client

These crates implement the contracts below orchestration:

- **`muta-llm-client`** — the multi-protocol HTTP client. Owns the pooled
  transport (`Client`, `Endpoint`, SSE byte reassembly, retry/error
  classification) and one module per wire protocol
  (`protocol::{openai, anthropic, google}`) holding the per-vendor
  request/response semantics. Each provider embeds a shared `Client` so a
  single connection pool is reused across every turn.
- **`muta-providers`** — the channel registry and `build_provider_for_channel`
  factory, live model-list discovery, and the OAuth2/PKCE flows for
  subscription providers. It selects *which* backend to talk to;
  `muta-llm-client` knows *how*.
- **`muta-skills`** — skill metadata, discovery, remote caching, registry,
  periodic refresh, and `use_skill` / `list_skills` tool adapters. The agent
  consumes the registry for model-context injection; Session also reads and
  refreshes it without reaching through Agent internals.
- **`muta-persistence`** — durable state: SQLite database, `SessionStore`, `Config`,
  repeat store, XDG paths (ADR-0014).
- **`muta-mcp`** — the MCP connector (ADR-0060, re-extracted by ADR-0098):
  stdio JSON-RPC transport, server lifecycle, tool adapters, the live
  `McpRuntime`, and catalog refresh. A session (in `muta-runtime`) owns each
  runtime; discovered tools reach the agent through the `DynamicToolSink`
  contract, and workspace trust is injected via a callback, so `muta-mcp`
  carries no dependency on `muta-persistence` or agent orchestration.
- **`muta-platform`** — the OS platform abstraction layer (PAL): process tree
  containment, native shell dialects and quoting, local IPC (UDS/Named Pipes),
  cross-platform clipboard, secure file replacement, and workspace sandboxing.

### `muta-agent` — orchestration

The engine. `Agent` + the round/turn loop (ADR-0047), model-request and
system-prompt policy, durable conversation-context injection, tool-call
dispatch and compatibility parsing, context projection, shell input policy,
`ProxyProvider`, skill context injection,
`EnvoyTool`, and the full-duplex envoy registry (ADR-0029). This crate knows how
to run *one* LLM round with tools. It also owns the built-in tools
(`execute_command`, `read_text`, `find_files`, `search_text`, `read_url`, todo management, …) in its
`tools` module: most self-register via `inventory`, and stateful todo tools
receive an agent-owned context bound in `tool_integration`. It consumes
`muta-skills` and interacts
with static and dynamic tools through core contracts. It does not know about
sessions, slash commands, or frontends. Identity-agnostic and
role-agnostic by design.

The `agent -> skills` edge is intentional layering, not a
cycle: the skills crate does not depend on agent orchestration.
`EnvoyTool` remains in Agent because it constructs and controls agents.

### `muta-runtime` — session runtime & control plane

The layer that turns "an engine that can run a turn" into "a running agent
session a frontend can drive." It owns:

- **`SessionDriver`** — owns one session's request receiver and long-lived
  state, then routes each `AgentRequest` to a handler until the channel closes.
- **Handlers** — `handlers_chat` / `handlers_permission` / `handlers_provider`
  / `handlers_session` / `handlers_slash`: one per `AgentRequest` group.
- **`/btw` side sessions** (`side`), **hooks**, **export**, **review**, **shell**.
- **`serve`** — the control-plane transport (ADR-0037 §7, generalized by
  ADR-0096): one WebSocket handshake serving the attach, monitor, and control
  roles over both TCP and a Unix domain socket.
- **`host`** — the unified daemon runtime (ADR-0096): one process owning every
  session across every project, plus the global discovery record and UDS
  listener.
- **`bootstrap`** — the session-harness assembly factory (ADR-0037 Step 6,
  landed by ADR-0081) that application binaries call; identity,
  principal profile, and `UiBridge` arrive as parameters.
- **`serve_discovery`** — the global discovery record the daemon writes so
  clients can find it (one per user, carrying the UDS path + TCP port), plus
  the legacy per-project path resolution it replaced (ADR-0096).
- **`slash_handler`** — the `SlashCommandHandler` extension point so embeddings
  register Rust slash commands without forking the server (ADR-0054).
- **`input_completion`** — the shared composer-completion policy: command and
  intent matching, aliases, content-admitted workspace commands, and `@path`
  discovery. Clients request ready-to-apply edits and do not duplicate these
  rules.
- **`UiBridge`** — the one frontend-capability trait (`/export` clipboard).
- **`client`** — the client side of the control plane (ADR-0098): discovery,
  the attach handshake, one-shot control verbs, and the monitor stream. Client
  and server speak the same `serve::Wire` protocol from the same crate, so the
  two cannot drift.

It depends on `agent` (downward) but **never on an application** (upward).
Frontends depend on it, never the reverse.

**Application-neutral.** Per ADR-0054, this crate holds no product name,
mission, or `PrincipalProfile`. The embedding supplies an `AgentIdentity` to
`Agent::new` and binds a `PrincipalProfile` via `apply_principal_profile`.
`/btw` side sessions reuse the primary agent's identity via `Agent::identity()`.

**One daemon, every session (ADR-0096).** The `SessionRegistry` hosts any
number of sessions in one process — each still its own writer under the
ADR-0018 invariant, indexed `project → session`. The ADR-0037 Step 6 factory
(`bootstrap::assemble`) assembles each hosted session on demand; the daemon
pays the assembly cost once per session, not once per process. The
per-project, one-server-per-session model of ADR-0081 is superseded.

### Core and app layer — `muta`, `apps/tui`, and `apps/web`

The user-facing presentation layers:

1. **`muta`** — the core binary. It owns daemon lifecycle and service commands
   (`auth`, `config`, `mcp`, `skill`, `session`, `daemon`, and `doctor`). It
   contains no frontend dependency or frontend assets.
2. **`apps/tui`** — one Rust app subproject containing `crates/mutx` and its
   private `crates/mutx-engine`. It owns interactive and headless prompt
   clients, attachment, the dashboard, clipboard behavior, and terminal
   rendering. It checks the daemon at startup and launches `muta` on demand,
   but cannot host the daemon.
3. **`apps/web/`** — the lightweight web frontend connecting directly to the session daemon's
   WebSocket listener, enabling browser-based fleet monitoring and agent chat. A pnpm
   workspace package (`pnpm-workspace.yaml`), not a Rust crate: Svelte 5 + TypeScript +
   Vite, with generated wire types from `muta-contracts` plus a small envelope
   adapter in `apps/web/src/lib/types.ts`. It builds and deploys independently;
   the daemon does not embed its output. Its lockfile is the root
   `pnpm-lock.yaml`.

## How a request flows across the layers

```
TUI keystroke / WS client
        │  AgentRequest (over mpsc, no source metadata)
        ▼
muta-runtime: SessionDriver  ──►  handlers_*  ──►  muta-agent: Agent::turn
        │                                                     │
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
- [ADR-0136](../adr/0136-muta-core-and-mutx-terminal-app.md) — the Muta rename
  and the core/TUI command split (current topology).
- [ADR-0081](../adr/0081-neenee-server-and-attach-model.md) — the server
  binary, the `bootstrap::assemble` factory, and the attach model.
- [ADR-0089](../adr/0089-multi-session-daemon.md) — the multi-session
  registry.
- [ADR-0093](../adr/0093-daemon-observability-monitor-protocol.md) — the
  monitor/observability protocol.
- [ADR-0094](../adr/0094-serve-as-host-verb.md) — the serve/host verb
  vocabulary.
- [ADR-0096](../adr/0096-unified-session-daemon.md) — the unified session
  daemon and control plane (current model).
- [ADR-0098](../adr/0098-crate-renames-and-library-extractions.md) — the
  earlier `contracts`/`host` renames and frontend extractions.
