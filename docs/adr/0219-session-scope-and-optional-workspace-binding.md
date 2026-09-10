# 0219. Session scope is not workspace: orthogonal conversation scope and optional workspace binding

- **Status:** Accepted
- **Date:** 2026-09-10
- **Followed by:** [ADR-0220](0220-personas-persisted-named-principals.md) — defines the `Persona` principal that this record's `SessionScope::Persona` names.
- **Supersedes (session partition key only):** ADR-0018 and ADR-0186, whose
  `sessions.project_root` is the sole history partition key. Their transcript
  model (ADR-0186), persistence mechanics (ADR-0187), and per-project
  concurrency (ADR-0018) survive unchanged.
- **Builds on:** ADR-0053 (declarative principal profile), ADR-0142 (additional
  workspace roots), ADR-0147 (orthogonal workspace security planes), ADR-0163 /
  ADR-0168 (authoritative SQLite ledger), ADR-0167 (worker-station model),
  ADR-0211 (agent roles and facets).

## Context

Every durable session is owned by exactly one `project_root`, a mandatory
filesystem path:

- `sessions.project_root TEXT NOT NULL` (migration 1; rebuilt in migration 5),
  indexed by `idx_sessions_project` and `idx_sessions_project_updated`.
- `SessionData.project_root: PathBuf` (`crates/muta-persistence/src/session/mod.rs`).
- `SessionStore::load_for_project` canonicalises a cwd and opens the store
  pinned to it (`crates/muta-persistence/src/session/store.rs`).
- Every history query filters on it: `list_session_summaries`,
  `resolve_session_prefix`, `search_history_inner` (`crates/muta-persistence/src/db.rs`).
- The daemon partitions its live session map by it: `HostedSession.project_root`,
  `resolve_auto`, `resolve_id`, `session_exists_on_disk`
  (`crates/muta-runtime/src/registry.rs`).

That single path simultaneously carries three unrelated meanings:

1. **Conversation partition key** — which history lane a session belongs to, and
   therefore what `list` / `resume` / search / lazy-resume return.
2. **Filesystem binding** — the tool root, sandbox boundary, workspace-trust
   scope, and the `network` / `debug` / `permissions` / `embeddings` assets.
3. **Physical bucket name** — `projects/<sha256(cwd)[..16]>`.

For a coding role the three coincidentally align, so the conflation stayed
hidden. `AgentRole` itself is already workspace-free
(`crates/muta-contracts/src/agent_preset.rs`): no path field, and
`apply_profile` never touches `project_root`. Workspace is bound later, at
assembly (`crates/muta-runtime/src/bootstrap.rs`), and an agent can run with no
project root at all.

The gap appears the moment a role is not about a repository. A conversational
role — a practice partner, a tutor, a research persona — needs a durable
conversation lane, but a cwd has no meaning for it. Today such a role has two
bad choices: borrow an arbitrary working directory as a fake lane, or lose
history entirely. The root cause is not "roles that need a workspace"; it is
that **the partition key was hard-coded to a concrete path type**.

## Decision

### 1. Two orthogonal axes

A session carries an always-present **conversation scope** (the partition /
identity key) and an optional, independent **workspace binding** (a capability
input). Neither implies the other.

```text
Session
├── scope: SessionScope                 // mandatory: which conversation lane
└── workspace: Option<WorkspaceBinding> // optional: where tools may operate
```

```rust
pub enum SessionScope {
    /// A filesystem workspace; the canonicalised project root is the key.
    Workspace(WorkspaceKey),
    /// A named, persistent non-filesystem identity (e.g. `english-practice`).
    Persona(PersonaKey),
    /// A private, unnamed lane that never appears in shared history lists.
    Ephemeral(ScopeUuid),
}

pub enum ScopeKind { Workspace, Persona, Ephemeral }

pub struct WorkspaceBinding {
    pub root: PathBuf,
    pub additional_roots: Vec<PathBuf>, // ADR-0142
}

impl SessionScope {
    /// Stable, namespaced partition key persisted in `sessions.scope_key`.
    pub fn key(&self) -> String;   // "ws:<canonical>" | "persona:<id>" | "ephemeral:<uuid>"
    pub fn kind(&self) -> ScopeKind;
    /// Filesystem bucket; only meaningful for asset directories.
    pub fn bucket(&self) -> String;
}
```

Scope keys are namespace-prefixed so a directory literally named
`persona:english` can never collide with a persona lane.

### 2. Scope replaces `project_root` as the sole partition key

The `sessions` schema drops `project_root` and gains `scope_kind`, `scope_key`,
`workspace_root`, and `additional_roots`:

```sql
-- sessions (relevant columns; ADR-0186/0187 columns unchanged)
scope_kind       TEXT NOT NULL CHECK (scope_kind IN ('workspace','persona','ephemeral')),
scope_key        TEXT NOT NULL,
workspace_root   TEXT,                              -- NULL for a workspace-free binding
additional_roots TEXT NOT NULL DEFAULT '[]',        -- JSON array (ADR-0142)
CREATE INDEX idx_sessions_scope ON sessions(scope_key, updated_at_s DESC);
-- project_root is deleted, not deprecated
```

`SessionData` mirrors this: `scope: SessionScope` plus
`workspace: Option<WorkspaceBinding>`, replacing `project_root: PathBuf`.

### 3. Workspace binding is restored from the session, not assumed from the caller

Because the binding is now an explicit stored field, opening a session from any
context restores **that session's own** workspace (or absence of one). A
persona session that happens to carry a workspace keeps it; a workspace-free
session never inherits a caller's cwd. This removes the current implicit
coupling between store construction and the agent's effective root.

### 4. Roles declare a requirement; they do not own a workspace

`AgentRole` gains a declaration, not a binding:

```rust
pub enum WorkspaceRequirement { Required, Optional, Forbidden }
```

- `code` / `code_analyst` / `architect` / `reviewer` / `security` keep
  `Required`.
- A conversational or practice role declares `Forbidden` (or `Optional`).

The runtime resolves a `SessionScope` when a session is created and validates it
against the role's requirement. A persistent, named principal — the "English
practice" identity — is a **Persona** with a stable id and a bound role, from
which `SessionScope::Persona(id)` is derived. Personas are a separate,
follow-up tranche; until they land, the mechanism is a session-start scope
selector taking a namespaced key.

### 5. Non-workspace scopes bypass workspace machinery

Persona and ephemeral scopes never trigger workspace trust, the permission
store, the sandbox, or `projects/<bucket>/{network,debug,permissions,embeddings}`.
Those assets are workspace-specific and remain keyed by the workspace bucket.
Sessions stay authoritative in `muta.db`; the per-bucket directories are not a
session key.

### 6. Global search becomes explicit

The existing `scope = None` query path is retained and re-labelled as a
deliberate cross-scope search (as the Archivist uses in ADR-0208). Scoped
queries filter `scope_key` exactly as they currently filter `project_root`.

## Invariants & Behavioral Boundaries

1. **One scope per session.** Every session row has exactly one
   `(scope_kind, scope_key)`; it is the only history partition key.
2. **Scope is never a workspace and a workspace is never a scope.** All history
   queries (`list`, `resolve_session_prefix`, search, lazy-resume, daemon
   auto-bind) filter `scope_key`, never `workspace_root`.
3. **Workspace binding is optional and independent.** A workspace scope without
   a binding and a persona scope with one are both lawful.
4. **Non-workspace scopes are trust-free.** No workspace-trust prompt,
   permission file, or sandbox applies when `workspace_root IS NULL`.
5. **`project_root` is deleted.** No column alias, accessor, or query fallback
   survives the migration.
6. **Caller context never overrides stored binding.** Opening a session uses the
   `scope` and `workspace` recorded on its row.
7. **The clean break is on schema, not on conversations.** Existing rows are
   backfilled mechanically (the transform is lossless); no user history is
   discarded.

## Migration

Bump `PRAGMA user_version` to 12. Rebuild `sessions` to the target shape, then
backfill every existing row:

```sql
UPDATE sessions
   SET scope_kind     = 'workspace',
       scope_key      = project_root,
       workspace_root = project_root;
```

Then `ALTER TABLE sessions DROP COLUMN project_root;` and recreate the scope
index. Unlike ADR-0186's semantic rewrite (which legitimately discarded legacy
snapshots), this transform is a lossless rename of one key into another, so the
conversation history is preserved. New non-workspace sessions start on the new
key immediately.

## Alternatives considered

- **Make `project_root` nullable.** Rejected. All workspace-free sessions would
  collapse into one `NULL` bucket, so distinct identities (a tutor, a practice
  partner, a research persona) would share one history lane. It also fails to
  name the real concept: the column would still be called a path while acting as
  an identity.
- **Add a parallel `persona_sessions` table or a second store.** Rejected.
  Every list / resolve / search / resume / daemon path would fork into two
  implementations, and two sources of truth would drift.
- **Use the agent role id as the partition key.** Rejected. A role is a runtime
  switch (`/role`), not a persistent identity; two personas sharing a role, or
  two coding sessions, would bleed into one lane.
- **Invent a hidden directory (e.g. `~/.config/muta/personas/english`) as a fake
  workspace.** Rejected. It reintroduces the path lie, drags persona sessions
  through workspace trust, permissions, and sandbox code they do not need, and
  makes "where does my history live" answerable only by reading the source.
- **Keep `project_root` and special-case the empty path.** Rejected. A sentinel
  empty path carries the same ambiguity as a nullable column plus a magic value,
  with worse failure modes under canonicalisation and hashing.

## Consequences

- `SessionData`, `SessionStore`, `DatabaseEngine`, and the daemon registry swap
  a `PathBuf` partition for a typed `SessionScope`; queries become uniform on
  `scope_key`.
- The workspace bucket becomes purely an asset-address space; session listing no
  longer depends on it.
- Role switching stays runtime-only and remains orthogonal to scope.
- A follow-up tranche introduces the persisted `Persona` entity and its CLI /
  config surface, at which point "create an English-practice agent identity" is
  a first-class operation rather than a scope key.

### Negative & mitigations

- **Blast radius across persistence, runtime, and daemon.** Every partition
  query and live-session comparison changes in one tranche. Mitigation: the
  schema migration is a single lossless backfill, and the new partition key is
  the old value for all existing rows, so behaviour is unchanged for every
  current workspace session.
- **Scope-key spelling becomes a compatibility surface.** Namespacing
  (`ws:` / `persona:` / `ephemeral:`) is fixed now because it is persisted.
  Mitigation: scope keys are opaque and namespaced; a future spelling change is
  a migration, not a query rewrite.

## References

- ADR-0018 — per-project multi-instance concurrency (bucket concurrency retained).
- ADR-0053 — declarative principal profile.
- ADR-0142 — additional workspace roots.
- ADR-0147 — orthogonal workspace security planes.
- ADR-0163 / ADR-0168 — unified SQLite ledger and single-source-of-truth storage.
- ADR-0167 — worker-station agent model.
- ADR-0186 — single-transcript persistence (`project_root` keying superseded).
- ADR-0187 — persistence v2 incremental append.
- ADR-0208 — cross-project session retrieval (explicit global search).
- ADR-0211 — agent roles, tools, and harness facets.
