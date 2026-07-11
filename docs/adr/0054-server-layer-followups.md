# 0054. Server-layer follow-ups: identity relocation, secure serve defaults, slash extension point

- **Status:** Accepted
- **Date:** 2026-07-14
- **Builds on:** ADR-0037 (server layer); ADR-0053 (declarative principal profiles)

## Context

ADR-0037 established `neenee-server` as the application-neutral session/transport
layer, and ADR-0053 placed the built-in coding principal (`principal_code()`) in
the server layer so "the layer that constructs agents" owned the identity. Three
gaps surfaced once the server was actually embedded and exercised:

1. **Identity coupling.** `principal_code()` + `neenee_identity()` + the
   `NEENEE_NAME`/`NEENEE_MISSION` constants lived in `neenee-server`. The
   `/btw` side-session builder (`handlers_slash.rs`) called `crate::neenee_identity()`
   to construct a side `Agent`, so the neutral server crate hard-coded a product
   name. A future `neenee-quant` binary would either inherit the wrong identity
   or have to fork the server — exactly the coupling ADR-0037 meant to prevent.

2. **Unsafe serve defaults.** `serve.rs` bound `0.0.0.0` unconditionally and had
   no authentication. ADR-0037 never discussed the bind address; the
   reference docs and AsyncAPI contract both documented "binds all interfaces,
   no authentication." A `/serve` typed casually exposed the full session
   transcript to the local network with no gate.

3. **No Rust slash-command extension point.** The 22 built-in commands live in a
   closed `define_builtin_commands!` macro + non-exhaustive `match` (correctly,
   so completion/help/dispatch never drift). The only custom-command mechanism
   was `.neenee/commands/*.md` markdown templates — prompt text, not Rust logic.
   An embedding that wanted `/backtest` to run code (not prompt the model) had
   to fork the macro and the match, modifying server source per application.

## Decision

### 1. Relocate identity to the application layer

`NEENEE_NAME` / `NEENEE_MISSION` / `neenee_identity()` / `principal_code()` move
from `neenee-server/src/lib.rs` to `neenee-code/src/identity.rs`. The server
crate is now identity-free: it holds no product name, mission, or principal
profile.

The `/btw` side session no longer names an identity itself. A new read-only
`Agent::identity() -> &AgentIdentity` getter lets the side-session builder reuse
the primary agent's identity (`agent.identity().clone()`) instead of calling a
server-level `neenee_identity()`. Since the primary was constructed with the
embedding's identity, the side inherits it automatically — and a future
`neenee-quant` binary's side sessions inherit the quant identity with zero
server changes.

The application supplies identity to `Agent::new` / `from_toolset` and binds a
`PrincipalProfile` via `apply_principal_profile`, both unchanged. The server
merely ceases to *name* them.

### 2. Secure serve defaults

`serve.rs` gains three public types:

```rust
pub enum ServeExpose { Local, Public }
pub struct ServeOptions { port, expose, token }
pub struct ServeHandle { port, cancel, token }
```

`start_server` takes `ServeOptions` instead of a bare `port`.

- **`Local` (default)** binds `127.0.0.1` and requires no token — a local
  co-process is trusted.
- **`Public`** binds `0.0.0.0` and **requires** a bearer token. The WebSocket
  handshake must carry `Authorization: Bearer <token>`, else it is rejected with
  401 before any session data is exchanged. If `Public` is requested without a
  token, `start_server` generates one (pid + time entropy, 32 hex chars) and
  surfaces it via `ServeHandle::token` so the caller can display it. A public
  port is never started unauthenticated.

The TUI `/serve` command parses `--public`: `/serve [port] [--public]`. Default
is loopback + no auth; `--public` switches to all-interfaces + forced token,
which the TUI prints so the user can hand it to a remote client.

### 3. `SlashCommandHandler` extension point

A new `slash_handler` module gives embeddings a first-class way to register
slash commands that run Rust logic:

```rust
#[async_trait]
pub trait SlashCommandHandler: Send + Sync {
    fn description(&self) -> &str;
    async fn handle(&self, ctx: SlashContext<'_>) -> bool;
}

pub struct SlashCommandRegistry { ... }  // register / get / list / is_empty
pub struct SlashContext<'a> { ... }       // the full dispatcher context slice
```

`Harness` gains an `extra_commands: Arc<SlashCommandRegistry>` field.
`handlers_slash::dispatch`'s `None` arm (unknown built-in) consults
`extra_commands` **before** the markdown-template fallback: a handler returning
`true` fully handles the command; `false` falls through to the template path.
`/help` lists registered handlers automatically.

This is the principal-side analogue of `inventory` tool self-registration:
capabilities (tools) and commands (slash handlers) are both supplied by the
embedding, never hard-coded in the neutral server. `neenee-code` registers none
today; a future `neenee-quant` binary can register `/backtest` etc. without
touching server source.

## Consequences

- **Server is application-neutral in full.** grep finds no product name,
  mission, or principal in `neenee-server/src/`. The crate's `lib.rs` documents
  an "Identity posture" section stating this explicitly.

- **Public serve is safe by default.** A casual `/serve` exposes nothing beyond
  loopback. Exposure is an explicit opt-in (`--public`) that cannot happen
  without a token.

- **Embeddings extend commands without forking.** The closed `BuiltinCmd` set
  stays closed (built-ins never drift); application commands are an open,
  runtime-registered set. The two coexist: built-ins always win on name
  collision (dispatch checks `BuiltinCmd::from_slash` first).

- **`Agent::identity()` is a new public getter** on a field that was
  `pub(crate)`. It is read-only and consistent with the existing "identity is
  immutable past construction" invariant.

- **ADR-0053's placement of `principal_code()` in the server layer is revised.**
  ADR-0053 is not edited (ADRs are immutable); this ADR supersedes that one
  placement decision. The rest of ADR-0053 (`PrincipalProfile` design,
  `apply_principal_profile`, `AgentIdentity` in core) stands unchanged.

- **`SessionRegistry::create_session` / `close_session` remain stubs** (ADR-0037
  Step 6, Pending). This ADR does not advance multi-session daemon support; it
  only addresses identity, serve security, and the slash extension point.

## Alternatives considered

- **Rename `neenee-server` to `neenee-code-server`.** Rejected: the server is
  ~95% application-neutral harness logic; only the ~15 lines of identity were
  code-specific. Renaming would declare 100% code ownership over 5% of the code
  and force every future application to duplicate ~4000 lines of driver logic.
  Relocating the identity achieves the same neutrality without the duplication
  tax.

- **Make `Public` serve tokenless and document "use a reverse proxy."** Rejected:
  defense in depth. A dev port should not rely on the operator remembering to
  firewall or proxy it; the token is enforced in-process at the handshake.

- **Add slash commands only via the `BuiltinCmd` macro (fork per app).**
  Rejected: that path requires editing server source for each application's
  commands, defeating the neutral-server goal. The registry + trait keeps
  built-ins closed (no drift) while letting applications register their own.

## References

- [0037](0037-server-layer.md) — the server layer this builds on.
- [0053](0053-declarative-principal-profile.md) — `PrincipalProfile`; this ADR
  revises only the placement of `principal_code()` (server → application layer).
- [0005](0005-strict-layering-and-renames.md) — strict DAG; the identity move
  preserves it (application supplies identity to the server, never the reverse).
- `crates/neenee-server/src/{lib.rs,serve.rs,slash_handler.rs}` — implementation.
- `crates/neenee-code/src/identity.rs` — the relocated identity.
- `crates/neenee-agent/src/agent.rs` — the `Agent::identity()` getter.
