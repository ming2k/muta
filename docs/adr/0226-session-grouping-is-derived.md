# 0226. Session grouping is derived (retire SessionScope; workspace + space)

- **Status:** Proposed
- **Date:** 2026-09-10
- **Supersedes:** [ADR-0219](0219-session-scope-and-optional-workspace-binding.md)'s `SessionScope` / `ScopeKind` model. The workspace-binding and lossless-migration decisions of ADR-0219 are retained.
- **Builds on:** [ADR-0225](0225-persona-owns-identity-and-capability.md).

## Context

ADR-0219 decoupled the session partition key from `project_root` by introducing
`SessionScope { Workspace(WorkspaceKey), Persona(PersonaKey), Ephemeral }`,
stored as a single namespaced `scope_key`. Two problems surfaced:

1. **Over-abstraction.** `ScopeKind`, namespaced keys, and `label()` elevate an
   internal partition key into a named domain concept. Users do not think in
   "lanes"; they think in projects and conversations.
2. **Persona conflation.** `SessionScope::Persona(id)` makes the persona *own*
   the history. But a persona is a switchable identity + capability
   (ADR-0225) — a hat, not a room. Tying grouping to it means switching
   identity would move or strand history.

Grouping is what actually matters: `list` / `resume` / search / isolation need
one unambiguous, derivable key. It does not need to be a stored, typed object.

## Decision

### 1. Concrete session fields, no scope type

```rust
pub struct Session {
    /// Where tools act. When present it is the grouping key.
    pub workspace: Option<PathBuf>,
    /// Explicit, user-named conversation space for workspace-free sessions.
    pub space: Option<String>,
    // persona: AgentPersona — identity + capability, orthogonal to grouping.
}
```

### 2. Grouping is derived

```
grouping_key = workspace_root  ??  space  ??  Personal
```

`Personal` is a single implicit space for workspace-free sessions that name
none. A persona never contributes to the grouping key.

### 3. Non-workspace separation is explicit

A workspace-free session that wants its own space names it (`mutx --space
philosophy`). A persona may declare a **default space** as a convenience, but
the space is the user's label, not the persona's identity: two personas may
share a space, and deleting a persona never affects a space.

### 4. Storage

DB v13 removes `scope_kind` / `scope_key` and adds `space`:

```sql
-- sessions
workspace_root   TEXT,          -- retained from ADR-0219; grouping when present
space            TEXT,          -- grouping for workspace-free sessions
-- scope_kind / scope_key dropped
```

Grouping queries filter on `workspace_root = ?1 OR (workspace_root IS NULL AND
space = ?2)` (and `Personal` for both null). Existing rows migrate as
`workspace_root` from `scope_key` when it is a workspace scope; persona /
ephemeral rows migrate to `space = 'Personal'` (their histories are preserved,
not re-keyed to a persona).

### 5. `SessionStore` is pinned to a grouping, not a scope

`SessionStore::for_grouping(workspace, space)` replaces `for_scope`.

## Invariants & Behavioral Boundaries

1. **Grouping is derived, never a stored opaque object.** No `SessionScope`,
   `ScopeKind`, or `scope_key` survives.
2. **Persona is orthogonal to grouping.** No persona id appears in the grouping
   key; switching persona never moves history.
3. **Workspace-first.** When a workspace is bound, it alone is the grouping key
   (workspace-free subdivision does not apply).
4. **`space` is user-owned.** Deleting/re-adding personas never affects a
   space or its history.
5. **Lossless migration.** Existing workspace sessions keep their grouping;
   existing non-workspace sessions land in `Personal`.

## Alternatives considered

- **Keep `SessionScope`.** Rejected. Over-abstracted and conflates persona with
  grouping.
- **Merge persona and grouping (persona owns the lane).** Rejected. Switching a
  persona would move history; a workspace-bound persona would have two owners.
- **No grouping for workspace-free sessions (one global recency list).**
  Considered; rejected in favour of an explicit, optional `space` so a user can
  separate topics without inventing a workspace.
- **Single `Personal` space only.** Kept as the default; `space` is the opt-in
  refinement.

## Consequences

- `SessionScope`/`ScopeKind`/`WorkspaceKey`/`PersonaKey`/`EphemeralKey` and
  `scope_key` are deleted; `workspace` + `space` are the concrete fields.
- Registry indexes and auto-bind key on the derived grouping.
- `personas.toml` no longer defines a lane; it may carry a default space.
- ADR-0219's scope model is superseded; its workspace-binding and migration
  guarantees stand.

## References

- ADR-0219 — session scope and optional workspace binding (superseded).
- ADR-0220 — persistent personas.
- ADR-0225 — persona owns identity and capability.
