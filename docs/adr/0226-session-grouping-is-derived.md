# 0226. Session partition is the workspace; persona is metadata (no scope type)

- **Status:** Proposed
- **Date:** 2026-09-10 (revised 2026-09-10)
- **Supersedes:** [ADR-0219](0219-session-scope-and-optional-workspace-binding.md)'s `SessionScope` model. The workspace-binding and lossless-migration guarantees are retained.
- **Builds on:** [ADR-0220](0220-personas-persisted-named-principals.md), [ADR-0225](0225-persona-owns-identity-and-capability.md).

## Context

Sessions all live in one global SQLite database, so history must be partitioned:
`list` / `resume` / search need an unambiguous "which sessions belong together".

ADR-0219 modelled that partition as a first-class `SessionScope` enum
(`Workspace` / `Persona` / `Ephemeral`) persisted as a namespaced `scope_key`.
Two problems surfaced:

1. **Over-abstraction.** A named, typed "scope/lane" with kinds and prefixes
   elevated an internal partition key into a user-facing concept. The partition
   is really just "which workspace, if any".
2. **Persona conflation.** `SessionScope::Persona(id)` made the persona *own*
   the history, contradicting the persona-as-switchable-principal model
   (ADR-0225). A persona is a hat, not a room.

A follow-up iteration added a user-configured `space` + `Named` variant so
workspace-free personas could separate their histories. That too was rejected:
it invented a partition concept for a case (`workspace = none`) that needs none,
and required the user to hand-author a namespace.

## Decision

### 1. The partition is the workspace, and nothing else

```rust
pub struct Session {
    pub workspace: Option<WorkspaceBinding>, // Some = a project; None = unbound
    pub persona: Option<String>,             // metadata: which principal staffed it
}
```

A session bound to a directory partitions by that directory; a session with no
workspace is simply **unbound** (`workspace_root IS NULL`). There is no scope,
no lane, no space, no `Named`, no `Personal` object — "unbound" is the absence
of a workspace, not a named bucket.

### 2. No partition type

`SessionScope`, `ScopeKind`, `scope_key`, `SessionGrouping`, `space`, and the
`Named`/`Personal` variants are deleted. The only remaining notion is a query
filter:

```rust
pub enum WorkspaceFilter { Any, Path(PathBuf), Unbound }
```

`WorkspaceFilter` is the shape of a `WHERE` clause (any / a specific workspace /
the unbound set), not a domain entity.

### 3. Persona is metadata, used for resume

`persona` records which principal staffed a session. It does **not** partition
history and never appears in the grouping key. It exists so a workspace-free
conversation can be found again and its identity restored on resume.

### 4. Resume semantics

- Default: a new session.
- `--resume`: the most recent session matching the request —
  - with `--persona <id>`: match `persona = id`, plus the resolved workspace
    (`inherit` → current directory, `fixed` → declared path) or unbound
    (`workspace = none`);
  - without a persona: the most recent session in the current workspace (or
    unbound) set.
  If no match exists, a new session is created.

On resume the stored `persona` restores the principal's identity (otherwise a
resumed persona session would lose its identity).

### 5. Storage (schema v14)

`sessions` drops `space` and adds `persona`:

```sql
-- retained: workspace_root TEXT, additional_roots TEXT
persona TEXT
-- dropped: scope_kind, scope_key (v13), space (v14)
CREATE INDEX idx_sessions_persona ON sessions(persona, workspace_root, updated_at_s DESC);
```

Migration is lossless for the partition: workspace sessions keep their
`workspace_root`; every non-workspace session becomes unbound.

## Invariants & Behavioral Boundaries

1. **Partition = workspace.** A session's partition is its workspace root, or
   unbound. No other key exists.
2. **No partition type.** No `SessionScope` / `SessionGrouping` / `space` /
   `Named` / `Personal` concept survives.
3. **Persona is metadata.** It never contributes to the partition; deleting a
   persona never affects a session's partition.
4. **Unbound is the absence of a workspace**, not a named bucket.
5. **Resume restores identity.** A resumed session re-applies its recorded
   persona.
6. **Lossless migration.** Workspace grouping is preserved across v13 → v14.

## Alternatives considered

- **Keep `SessionScope` (Workspace/Persona/Ephemeral).** Rejected:
  over-abstracted; conflates persona with grouping.
- **Keep a user-configured `space`/`Named`.** Rejected: a partition concept
  invented for a case that needs none; user-authored namespace with no domain
  meaning.
- **Group workspace-free conversations by persona (persona-owned lane).**
  Rejected: switching a persona would move or strand history; a workspace-bound
  persona would have two owners.
- **A `SessionGrouping { workspace, space }` value type.** Rejected: two
  `Option`s that cannot express "workspace wins" without normalization; a
  single workspace binding suffices.

## Consequences

- `SessionData` carries `workspace: Option<WorkspaceBinding>` and
  `persona: Option<String>`; `SessionStore` is pinned to an optional workspace.
- Every history query filters on `workspace_root` (`Any` / `Path` / `Unbound`).
- `mutx --resume` (with optional `--persona`) resumes; default is new.
- ADR-0219's scope model is superseded; its workspace-binding and migration
  guarantees stand. ADR-0220's `personas.toml` no longer carries `space`.

## References

- ADR-0219 — session scope and optional workspace binding (superseded).
- ADR-0220 — persistent personas.
- ADR-0225 — persona owns identity and capability.
