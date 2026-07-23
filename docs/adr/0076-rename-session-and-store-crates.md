# 0076. Rename `neenee-session` → `neenee-transport` and `neenee-store` → `neenee-persistence`

- **Status:** Accepted
- **Date:** 2026-07-23
- **Companion to:** [ADR-0075](0075-rename-neenee-code-to-neenee.md) (the application
  crate rename) — this ADR renames the two remaining imprecisely-named crates.

## Context

After ADR-0073 flattened the workspace and ADR-0075 renamed the sole application
crate `neenee-code` → `neenee`, two crates still carried names that did not
describe what they actually do. Both are vocabulary problems, not architecture
problems — the strict-DAG topology from ADR-0005 is correct and unchanged.

### `neenee-session` — a name that collides with a module it does not contain

This crate's name history is:

| Era | Name | Why |
|-----|------|-----|
| ADR-0037 (original) | `neenee-server` | The planned multi-session daemon layer. |
| Later rename (see `CHANGELOG.md`) | `neenee-session` | "The vocabulary it defines (`SessionDriver`, `SessionRegistry`, …) already centered on session; the crate name now matches." |
| This ADR | `neenee-transport` | See below. |

The `neenee-session` name created a **semantic collision**: the durable,
event-sourced session store lives in `neenee-store/src/session/` (the
`SessionStore` facade, the JSONL event log, snapshots, fork/resume). A reader
who opens `neenee-session` expecting to find session *storage* finds none of it
— `neenee-session` owns the *runtime* that drives a live session (the request
loop, handlers, the `/serve` WebSocket bridge), and depends on `neenee-store`
for the durable half. Two different things both called "session" is the classic
overload smell.

Meanwhile the crate describes itself, in its own `lib.rs` and `README.md`, as
the **transport** between orchestration and frontends:

> owns the long-lived agent session, multiplexes requests/responses, and exposes
> a stable **transport** so any frontend can attach.

The word the crate uses for itself is "transport", not "session".

### `neenee-store` — a name that covers only half the crate

`neenee-store` holds: config loading (`config.rs`) and path resolution
(`paths.rs`); the event-sourced session store; blob storage; the embedding
index; the per-project advisory lock (`flock`); model-usage telemetry; and the
SQLite repeat/cron store. "store" describes the storage half (events, blobs,
embedding, cache, repeat) but not config, paths, locks, or telemetry. A new
reader seeing `neenee-store` expects a storage abstraction (a KV, blob, or cache
interface), not "the local agent's entire durable state plus configuration plus
infrastructure layer".

### The two renames compose

Renaming only one would leave the smell half-fixed. Renaming both lets the word
"session" mean **exactly one thing** across the workspace: the persisted
conversation in `neenee-persistence::session`. The layer diagram then reads
accurately:

```text
neenee ──► neenee-transport ──► neenee-agent ──► neenee-tools ──► neenee-persistence
```

## Decision

Rename two crates, pure rename — no responsibility, dependency, or topology
change:

| Old | New | Rationale |
|-----|-----|-----------|
| `neenee-session` (crate) | **`neenee-transport`** | Matches the crate's own self-description ("a stable transport so any frontend can attach"). Frees "session" to mean only the persisted conversation. |
| `crates/neenee-session/` (dir) | **`crates/neenee-transport/`** | Directory matches package name. `git mv` preserves history. |
| `neenee-store` (crate) | **`neenee-persistence`** | Accurately spans storage *and* config/paths/locks/telemetry — everything durable the local agent writes to disk. |
| `crates/neenee-store/` (dir) | **`crates/neenee-persistence/`** | Directory matches package name. `git mv` preserves history. |

Internal type names that use the word "session" (`SessionDriver`,
`SessionRegistry`, `SessionHandle`) are **not** renamed by this ADR. They are
public API referenced widely, and once qualified by the crate path
(`neenee_transport::SessionDriver`) they no longer claim the whole concept — a
crate named "session" does, a type named `SessionDriver` inside a crate named
"transport" does not. Renaming the types is a separate, larger decision and is
not required to fix the crate-level collision this ADR targets.

Nothing else about the topology changes. The strict-DAG property from ADR-0005,
each crate's responsibilities, and every dependency edge are unchanged.

## Alternatives considered

- **Delete `neenee-session` and distribute its modules across other crates.**
  Rejected. The crate is ~6,600 lines of strongly cohesive code — everything
  about turning "an engine that runs one turn" into "a live session a frontend
  can drive": `SessionDriver`, five handler groups, the `/serve` WebSocket
  transport, `/btw` side sessions, MCP runtime ownership, pursuits, hooks,
  export, review, shell. Merging it down into `neenee-agent` would violate
  ADR-0005's strict DAG (Agent is explicitly session/slash/frontend-agnostic and
  identity-agnostic) and pull WebSocket transport into the single-turn engine.
  Merging it up into the application crate reverses ADR-0037's reason for
  existing (a single-process model cannot serve a browser frontend) and kills
  the multi-frontend/multi-session roadmap. Distributing modules across several
  crates has no natural seams and would shred the request-dispatch loop. The
  crate's *existence* is justified; only its *name* was wrong.

- **Rename `neenee-session` back to `neenee-server` (the original ADR-0037
  name).** Rejected. "server" overpromises a multi-session daemon that is not
  built yet — today there is exactly one session per process, and the
  `SessionRegistry`/`SharedState` scaffolding was removed as dormant. "transport"
  matches what the crate is and does now, not what a future daemon might be.

- **Rename `neenee-session` to `neenee-harness`.** Rejected. `neenee-harness`
  was the **old name of `neenee-agent`** (renamed by ADR-0005). Reusing it for a
  different crate would create a historical-collision ambiguity: "which crate was
  `neenee-harness`?" See the legacy-terms table in `glossary.md`.

- **Rename `neenee-store` to `neenee-db` / `neenee-state` / `neenee-storage`.**
  All rejected. `db` is too narrow (ignores config/paths/locks). `state` implies
  in-memory runtime state, which lives in `neenee-agent`. `storage` has the same
  half-coverage problem as `store`, just longer. `persistence` is the only word
  that spans storage + config + paths + locks + telemetry.

- **Rename `neenee-store` to `neenee-persistence` but keep `neenee-session`.**
  Rejected: leaves the `neenee-session` ↔ `neenee-persistence::session`
  collision intact, which is the more confusing of the two smells.

- **Also rename the internal `Session*` types.** Out of scope (see Decision).
  Those types are stable public API; renaming them is a separate breaking change
  that the crate-level rename does not require.

## Consequences

- **Positive.** Each crate name now describes what it contains. The layer
  diagram is self-explaining without a glossary.

- **Positive.** The word "session" now has exactly one meaning across the
  workspace: the persisted conversation in `neenee-persistence::session`
  (`SessionStore`, the event log, snapshots). Readers no longer have to disambiguate
  "which session — the runtime crate or the storage module?".

- **Positive.** The renamed crate matches its own documentation: `lib.rs` and
  `README.md` already called it "the transport layer".

- **Negative (one-time, breaking).** Every `neenee-session` / `neenee-store`
  path dependency and every `use neenee_session::` / `use neenee_store::`
  reference must update to the new names. This is a workspace-internal rename;
  the crates are not published, so the blast radius is this repository plus any
  out-of-tree embedding. Recorded under `[Unreleased]` → `Changed` in
  `CHANGELOG.md`.

- **Neutral.** `Cargo.lock` is reconciled by `cargo build`. The internal
  `Session*` type names are unchanged, so the rename is mechanical at the crate
  boundary.

## Migration mechanics

| What | Files | Notes |
|------|-------|-------|
| `git mv` directories | `crates/neenee-session/` → `crates/neenee-transport/`, `crates/neenee-store/` → `crates/neenee-persistence/` | history preserved |
| package names | both crates' `Cargo.toml` | `name = "…"` |
| path dependencies | every consuming `Cargo.toml` (`neenee`, `neenee-agent`, `neenee-tools`, `neenee-skills`, `neenee-auth`, `neenee-transport`) | `neenee-store`/`neenee-session` keys + `path = "…"` |
| `use` + path references | `.rs` files across the workspace | `neenee_session`/`neenee_store` → `neenee_transport`/`neenee_persistence` |
| lockfile | `Cargo.lock` | package names; reconciled by `cargo build` |
| crate READMEs + doc comments | across crates | titles + prose |
| living docs | `docs/explanation/`, `docs/reference/`, `docs/how-to/`, `docs/dev/` (excl. `docs/dev/documentation/` policy) | mechanical rename + prose fixes |
| glossary | `docs/reference/glossary.md` | new `neenee-transport` term; legacy-terms rows for the old names |
| ADR index | `docs/adr/index.md` | new row only |

ADR decision bodies (0005, 0014, 0017, 0032, 0037, 0048, 0060, …) still contain
`neenee-session` / `neenee-store` references. Per ADR workflow they are immutable
historical records and are left unchanged; this ADR and the glossary carry the
current truth.

## References

- [ADR-0005](0005-strict-layering-and-renames.md) — the strict-DAG topology and
  the `neenee-app` → `neenee-store`, `neenee-harness` → `neenee-agent` renames;
  the layering invariant this ADR preserves.
- [ADR-0037](0037-server-layer.md) — created the server/session layer as
  `neenee-server`; the historical first name of this crate.
- [ADR-0075](0075-rename-neenee-code-to-neenee.md) — the companion application
  rename; same date, same single-product vocabulary cleanup.
- [Crate layering](../explanation/crate-layering.md)
- [Workspace layout](../dev/workspace-layout.md)
