# 0081. Standalone `neenee-server` binary and the attach model

- **Status:** Accepted
- **Date:** 2026-07-23
- **Builds on:** ADR-0037 (server layer), ADR-0018 (per-project
  multi-instance concurrency)
- **Revises:** ADR-0037's packaging choice ("server embedded in
  `neenee-code`", with a standalone binary deferred as "a future option")

## Context

ADR-0037 created the session/transport layer and considered a standalone
daemon binary, but rejected it at the time: "the user explicitly chose the
'server embedded in neenee-code' packaging… a standalone `neenee-serverd`
binary remains a future option… it would just be a `main.rs` consuming the
same library." The multi-session `SessionRegistry`/`SharedState`
scaffolding was later removed as dormant; the only sharing mechanism that
shipped was `/serve` — hot-attaching WebSocket clients to the
TUI-hosted session (`crates/neenee-transport/src/serve.rs`).

The requirement is now concrete: **`neenee-cli` and a server process must
be able to work simultaneously on the *same* live session**, with the
server spawned on demand (passively enabled), not run as a manually
managed daemon.

Two architectural facts constrain the shape:

1. **ADR-0018's one-writer invariant.** Every live session is a
   self-contained `sessions/<id>.{json,jsonl}` with exactly one writer;
   there is no shared on-disk "active session" pointer. Two processes
   cannot each *host* the same session — the seq and snapshot races that
   motivated ADR-0018 would return. Sharing a live session therefore means:
   **one process hosts it; every other process attaches as a client.**
2. **Co-driving was already designed in.** `AgentRequest` carries no
   source/client field, so a request from the TUI and a request from a
   WebSocket client are indistinguishable to `SessionDriver`
   (`docs/explanation/crate-layering.md`). `AgentRequest`/`AgentResponse`
   have been serde-safe since ADR-0037 step 0. The `serve.rs` protocol
   (History replay on connect + live Response broadcast + Request inbound)
   already turns one session into a multi-client stream.

What was missing: a headless host process, a way for clients to find it,
and a client mode in the TUI.

## Decision

### 1. Land the ADR-0037 Step 6 factory: `neenee_transport::bootstrap`

The ~410 lines of session assembly that lived in `neenee-cli/src/main.rs`
(config migrations, provider/ProxyProvider, skills, toolset, `EnvoyTool`,
MCP background start, permission/hook/principal wiring, session restore,
`SessionDriver` construction) moved into
`crates/neenee-transport/src/bootstrap.rs`:

```rust
pub struct BootstrapParams {
    pub identity: AgentIdentity, pub principal: PrincipalProfile,
    pub ui: Arc<dyn UiBridge>, pub startup: StartupMode,
    pub project_root: Option<PathBuf>,
    pub unattended: bool, pub single_instance: bool,
}
pub async fn assemble(params: BootstrapParams) -> Result<Bootstrap, …>
```

Both binaries call it. Transport stays application-neutral (ADR-0054):
identity/principal/`UiBridge` arrive as parameters. The opt-in
`ProcessLock` guard is returned in `Bootstrap` so the caller holds it for
the process lifetime. `assemble` also creates the XDG app roots up front,
fixing first-run-on-fresh-XDG for both binaries (previously
`RepeatStore::open` failed on a missing parent). This retires the
documented "reach-through" interim state — `neenee-cli` no longer depends
on `neenee-tools`/`neenee-mcp` directly, and its `main.rs` is 115 lines.

### 2. `crates/neenee-server`: a thin headless host

New application-layer binary (peer of `neenee-cli`; package and command
`neenee-server`). It parses `--project <path>` / `--session <id>` /
`--port <n>` (default 0) / `--public`, calls `bootstrap::assemble`
(`Fresh` or `Resume(id)`), drains the driver's response channel into a
`broadcast::channel(1024)`, and calls `serve::start_server`. It holds one
`req_tx` clone for the process lifetime (SessionDriver exits when all
request senders drop), prints one startup line
(`project=… session=… listening=…`), fires SessionEnd hooks on shutdown,
and exits on SIGINT/SIGTERM. Its `identity.rs` mirrors
`neenee-cli`'s (both binaries are the same product's application layer;
ADR-0054) and its `UiBridge` is a headless no-op.

The name reuses the word ADR-0076 deliberately freed: "server" was
rejected as the *library* name only because "no daemon exists yet" — the
binary is exactly the missing referent. The library keeps the honest name
(`neenee-transport`); the process that serves gets "server".

### 3. Discovery: how clients find the server

`neenee_transport::serve_discovery`: after binding, the server writes
`$XDG_RUNTIME_DIR/neenee/serve/<project-bucket>.json` (fallback: the
project bucket dir itself) containing `{pid, port, token, session_id,
project_root, started_at}`, atomically (temp + rename, 0600 — it can carry
the bearer token). Removed on clean shutdown. A second server for the same
project overwrites with a warning (last server wins; v1 supports one
server per project bucket).

### 4. `neenee --attach [session-id]`: the TUI as a client

`crates/neenee-cli/src/remote.rs`:

- `discover()` — reads the record, validates liveness by TCP connect
  (dead → remove file, treat as absent).
- `ensure_server()` — **passive enablement**: with no live server, spawn
  `neenee-server` (sibling of the current exe, else PATH) and poll the
  discovery file for up to 10s. A live server hosting a *different*
  session than requested is an explicit error naming the hosted session.
- `connect()` — WebSocket with `Authorization: Bearer` when the record has
  a token; consumes the `History` frame; then bridges both directions so
  the TUI sees the same `(UnboundedSender<AgentRequest>,
  UnboundedReceiver<AgentResponse>)` pair as the standalone path.
  **Zero changes to the TUI's channel interface.**

`Wire::History` gained a `session_id` field (the only protocol change).
The TUI's two direct `SessionStore` couplings are replaced by
`SessionSource::{Local, Remote{session_id}}`: the per-frame `session.id()`
uses the handshake id in remote mode; `/serve` and the token-source modal
degrade to notices in remote mode (the token ledger is an in-process
handle and is not a protocol concept).

Standalone mode is byte-for-byte the old path: in-process driver, mpsc,
no serialization — ADR-0037 §6's reasoning (no `Clone` tax on
`ToolResult`s for the dominant single-client case) is preserved.
Attach mode is opt-in.

## Alternatives considered

- **Two independent processes sharing on-disk session state.** Rejected:
  reintroduces the ADR-0018 `seq`/snapshot races; two live
  `SessionDriver`s on one session is the exact corruption that ADR
  removed.
- **Duplicate the assembly in the server instead of extracting
  `bootstrap`.** Rejected: ADR-0037 already rejected forking the driver
  for the same reason — the two wiring paths drift on every
  dispatch/permission change. The smoke run immediately proved the point:
  the first-run XDG gap would have had to be fixed in two places.
- **`neenee-serve` as the binary name** (matching `/serve` and
  `serve.rs`). Rejected by the user in favor of `neenee-server`; with
  ADR-0076's vocabulary history the name is free and now truthful.
- **Make attach the default TUI mode.** Rejected: puts serialization,
  clone costs, and a second process on the dominant single-client path,
  which ADR-0037 §6 deliberately avoids.
- **Multi-session server (revive `SessionRegistry`).** Deferred again:
  one server hosts one session; multiple sessions are multiple spawned
  servers (per-project bucket limit of one is the v1 simplification, not a
  design ceiling). The registry remains the right next step if a daemon
  ever needs N sessions in one process.

## Consequences

- **Positive.** Two binaries (and any number of browser/WS clients) can
  co-drive one live session: `neenee-server` hosts, `neenee --attach` and
  WS clients attach. History/state are shared through the project bucket
  for every process, per ADR-0018.
- **Positive.** The assembly exists once. `neenee-cli`'s dependency
  reach-through (documented as interim in `crate-layering.md`) is gone.
- **Positive.** `/serve` hot-attach and `neenee-server` are the same
  protocol and the same code path; no second wire format.
- **Negative — v1 limitations (accepted, recorded):**
  1. One server per project bucket; attaching to a different session id
     errors out naming the hosted session and pid.
  2. The client cannot see server-side custom slash commands (empty
     suggestions).
  3. Provider/model labels start as placeholders until the first
     server-driven snapshot arrives.
  4. Token-source report and `/serve` are notices-only in attach mode.
  5. Server disconnect ends the attached TUI (synthesized
     `AgentResponse::Exit`); no reconnect.
  6. `/exit` from one client broadcasts `Exit` to all attached clients;
     the server keeps running.
  7. Discovery liveness is a TCP probe; the WS handshake is the real
     validation.
  8. `--unattended`/`--single-instance` parse alongside `--attach` but do
     not apply (server-side concerns).
- **Neutral.** Crate count: 13 → 12 (ADR-0079) → 13 (this ADR).
- **Migration.** None for users: the `neenee` command and standalone
  behavior are unchanged. `neenee-server` ships as a second binary.

### Verification at landing

- `cargo test --workspace`: 1471 passed; clippy
  `--workspace --all-targets -- -D warnings` clean.
- Cross-process smoke (isolated XDG, temp project): server starts on a
  fresh XDG root, writes discovery, WS handshake returns `101` +
  `History`; the attach client discovers, connects, sends a `Chat` that
  reaches the live `SessionDriver` (NoProvider sentinel replies), and
  receives the response; SIGINT removes the discovery file, exit 0.
  Wrong-session attach fails with the one-server error. No-server attach
  spawns the server via the sibling-exe rule.
- The interactive TUI-over-attach path has no headless test harness (raw
  mode needs a tty); it is covered up to the terminal boundary by the
  cross-process smoke and the in-process bridge integration tests
  (history/session-id handshake, both bridge directions, bearer-token
  enforcement).

## References

- [ADR-0037](0037-server-layer.md) — the server layer; §6's mpsc
  standalone path is preserved, and its deferred "standalone binary"
  option is what this ADR lands.
- [ADR-0018](0018-per-project-multi-instance-concurrency.md) — the
  one-writer-per-session invariant that forces the host/clients shape.
- [ADR-0054](0054-server-layer-followups.md) — application neutrality of
  the transport layer; identity lives in each binary.
- [ADR-0076](0076-rename-session-and-store-crates.md) — the name history
  that freed "server".
- [ADR-0080](0080-rename-neenee-to-neenee-cli.md) — the peer rename.
- [Server WebSocket API](../reference/server-api.md) —
  protocol reference (updated for `History.session_id`).
