# 0186. Single-transcript persistence with projection directives (clean-break session schema)

- **Status:** Accepted; persistence-mechanics claims (event ledger, replay,
  `applied_seq`) are superseded by [ADR-0187](0187-persistence-v2-incremental-append-and-blob-reference-ledger.md)
- **Date:** 2026-09-06
- **Supersedes (persistence data model only):** ADR-0040 (vocabulary survives; the
  `model_window` / `archived_transcript` dual-array storage it named is replaced),
  the `SessionData` snapshot schema introduced by ADR-0016/0048 evolutions, and
  the `messages` materialized table from ADR-0163.

## Context

The current durable session (`crates/muta-persistence/src/session/mod.rs`,
`SessionData`) stores the transcript as **two mutable arrays**:

- `model_window: Vec<Message>` — what the next provider request starts from,
- `archived_transcript: Vec<Message>` — originals moved out by pruning or
  compaction,
- plus `last_projection: Option<ContextProjectionCheckpoint>` describing the
  most recent projection operation.

Every projection (prune/compact) is a compound mutation: messages are *moved*
between the two arrays, the checkpoint is inserted, and the checkpoint metadata
is rewritten. Derived state has leaked into the persistence layer. It forces a
class of defensive machinery —
"replay must not expand one projection into three operations" atomicity
rules, late-commit rejection, projection operation tags, checksum discipline —
whose only job is to keep two copies of the same fact from disagreeing. It also
duplicates every projected-out message (the archived copy), inlines nested
subagent transcripts recursively into parent messages, and cannot represent
fork sharing without copying.

Separately, mid-session provider switching surfaced a protocol problem
(Gemini's `thought_signature` validation rejects function calls replayed from
another model's history) whose correct fix — store per-message protocol
provenance and transform only at the provider boundary — requires the durable
store to keep *one* untouched factual transcript with provenance, not a
rewritten projection.

The design conversation settled on an event-sourced model: persist facts and
decisions only; derive views. This ADR records the resulting schema.

## Decision

### 1. Storage principle

The persistence layer stores **facts** (what happened) and **decisions**
(what the views should look like). It never stores a derived view. The model
window and the TUI presentation are pure functions of facts + decisions,
recomputed on load.

- **Transcript** — the only copy of history. Append-only; entries are never
  mutated, never moved between containers, never rewritten by projections.
- **Projection directives** — durable records of each projection *decision*
  (prune / compact / freeze), anchored to transcript positions. Appending a
  directive is the only effect a projection has on storage.
- **Working state** — current values with no historical semantics
  (provider connection pin, title, digest). Changes still emit events.
- **Events** — the single source of truth. All snapshot tables are a
  high-watermark materialization of the event ledger and can be dropped and
  rebuilt by replay.

### 2. Relational schema (SQLite, `PRAGMA user_version` 5)

The schema below replaces the JSON-blob `sessions.data` snapshot, the
`messages` materialized table, and its FTS triggers. **Clean break: legacy
snapshot payloads are not migrated and are discarded** (see Migration).

```sql
-- Sessions: identity + working state + snapshot bookkeeping.
CREATE TABLE sessions (
    id                  TEXT PRIMARY KEY,          -- uuid v7 (time-ordered)
    parent_id           TEXT REFERENCES sessions(id) ON DELETE SET NULL,
    fork_kind           TEXT NOT NULL DEFAULT 'trunk'
                        CHECK (fork_kind IN ('trunk','fork','aside','subagent')),
    created_at_ms       INTEGER NOT NULL,
    updated_at_ms       INTEGER NOT NULL,
    project_root        TEXT NOT NULL,

    -- working_state (current values; every change emits an event)
    provider_connection TEXT,                      -- connection-level pin; NULL = follow global
    model               TEXT,
    title               TEXT,                      -- non-NULL is terminal; AI generates only when NULL
    digest              TEXT,                      -- JSON resume working-memory summary
    digest_anchor       INTEGER,                   -- transcript watermark when digest was made
    disabled_tools      TEXT NOT NULL DEFAULT '[]',-- JSON array
    unattended          INTEGER NOT NULL DEFAULT 0,
    round_counter       INTEGER NOT NULL DEFAULT 0,

    -- snapshot bookkeeping
    applied_seq         INTEGER,                   -- highest folded event seq
    checksum            INTEGER,                   -- CRC32C over the canonical row payload
    schema_version      INTEGER NOT NULL,
    -- ledger-backed working state (projection/audit state per C11/C12)
    commands            TEXT NOT NULL DEFAULT '[]',-- JSON command ledger (ADR-0091)
    round_interrupts    TEXT NOT NULL DEFAULT '[]',-- JSON RoundInterrupt records
    retry_pending       TEXT,                      -- JSON RetryPoint, NULL when disarmed
    request_usage_records TEXT NOT NULL DEFAULT '[]', -- JSON per-request usage
);
CREATE INDEX idx_sessions_project ON sessions(project_root);
CREATE INDEX idx_sessions_updated ON sessions(updated_at_ms DESC);

-- Entries: immutable facts. No session ownership, no position.
CREATE TABLE entries (
    id          TEXT PRIMARY KEY,                  -- uuid v7, global identity
    kind        TEXT NOT NULL CHECK (kind IN ('message','state')),
    role        TEXT CHECK (role IN ('user','assistant','system','tool')),  -- message only
    content     TEXT,                              -- body text; separate column avoids JSON escaping
    origin      TEXT CHECK (origin IS NULL OR origin IN ('harness','checkpoint')),
    hidden      INTEGER NOT NULL DEFAULT 0,
    created_at_ms INTEGER NOT NULL,
    payload     TEXT NOT NULL,                      -- JSON, kind-specific:
                        --   message:   tool_calls[{id,name,arguments}], tool_call_id,
                        --              images, provider, model, effort,
                        --              provider_meta (opaque, protocol-owned),
                        --              subagent {session_id, description, duration_ms, toolset_count}
                        --   state:     { todos }  -- working-state snapshots (e.g. TodoList mirror)
    CHECK ( kind <> 'message' OR role IS NOT NULL )
);

-- Memberships: where an entry appears. Position belongs here, not to facts.
CREATE TABLE entry_memberships (
    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    seq        INTEGER NOT NULL,                   -- per-session monotonic, gap-free
    entry_id   TEXT NOT NULL REFERENCES entries(id),
    added_by   INTEGER NOT NULL,                   -- events.seq that created this row (replay anchor)
    PRIMARY KEY (session_id, seq)
);
CREATE INDEX idx_memberships_entry ON entry_memberships(entry_id);

-- Projection directives: the complete decision history. Append-only.
CREATE TABLE projections (
    session_id  TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    seq         INTEGER NOT NULL,                  -- directive's own total order
    kind        TEXT NOT NULL CHECK (kind IN ('prune','compact','freeze')),
    up_to_seq   INTEGER NOT NULL,                  -- anchor into memberships.seq
    payload     TEXT NOT NULL,                      -- JSON:
                        --   prune:   {"elided": [tool_call_id, ...]}
                        --   compact: {"checkpoint_seq": N}    -- the checkpoint is itself an entry
                        --   freeze:  {"shape": "..."}         -- byte-frozen wire shape record
    created_at_ms INTEGER NOT NULL,
    PRIMARY KEY (session_id, seq)
);

-- Blobs: content-addressed large payloads (spilled tool output, images).
CREATE TABLE blobs (
    hash      TEXT PRIMARY KEY,                    -- sha256
    size      INTEGER NOT NULL,
    mime      TEXT NOT NULL,
    data      BLOB,                                -- inline for small objects
    path      TEXT,                                -- external file for large ones
    CHECK ( (data IS NULL) <> (path IS NULL) )
);

-- Events: the single source of truth (renames session_events; same ledger role).
CREATE TABLE events (
    session_id    TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    seq           INTEGER NOT NULL,                -- per-session monotonic, gap-free
    event_type    TEXT NOT NULL,
    payload       TEXT NOT NULL,                   -- JSON, operation-complete
    created_at_ms INTEGER NOT NULL,
    PRIMARY KEY (session_id, seq)
);
CREATE INDEX idx_events_session_seq ON events(session_id, seq ASC);
```

### 3. Entry envelope

The envelope carries only columns consumed by indexes, invariants, or view
decisions: `id, kind, role, content, origin, hidden, timestamp`. Everything
kind-specific lives in `payload` JSON and is decoded into the typed
`TranscriptEntry` enum in `muta-contracts`. Per-kind field coherence is
enforced by the Rust decoder, not by SQL CHECK matrices.

- `origin` is **provenance** (how the message came to be): `NULL` genuine
  dialogue, `'harness'` program injection, `'checkpoint'` compaction summary.
- `hidden` is **view visibility** (does it enter the projected view). The two
  are orthogonal; `origin IS NOT NULL` implies `hidden = 1`.
- `tool_call_id` stays inside `payload`. The tool-pairing invariant is checked
  at admission time against the in-memory near-tail window, not via SQL.
- Round interrupts remain durable **projection state** carried by the event
  ledger (as ADR-0040/C11 established) — they are not transcript entries, since
  they are consumed (cleared) when a stopped round completes, which append-only
  entries cannot express.
- The `todos` working state is mirrored as `state` entries (latest wins on
  derivation) so the task list derives from the transcript like every other
  durable fact. `title_manual` is dropped — a non-NULL title is terminal and
  AI generation only fills `NULL`.

### 4. Views

- **Projected view** (the only view the persistence layer names): derived by a
  pure function from transcript + directives. Pruned tool results render as
  placeholders (originals in blobs), compacted ranges are replaced by their
  checkpoint entry, frozen shapes replay byte-identically, interrupts are
  excluded.
- All other read models (the TUI's presentation, doctor audits) derive from the
  same facts by their own rules; the persistence layer does not define them.
- Unknown entry/directive kinds are **preserved verbatim** by all readers and
  degraded gracefully by each view — never dropped, never an error.
- No SQL VIEW is part of the contract. The derive lives in one implementation.

### 5. Protocol-private state

Per-message provider-private data (Gemini `thought_signature` maps, Anthropic
`thinking_signature`) rides in the message's `provider_meta` payload slot. It
is written atomically with the message at admission, round-trips verbatim
through every projection, and is interpreted only by the protocol adapter that
produced it. Provider switching therefore transforms only at the send boundary
against the target protocol's capability profile; storage never rewrites
history. Each assistant message is stamped with its producing `provider` /
`model` so the boundary can attribute provenance.

### 6. Subagents

A subagent run is a **first-class session** (`fork_kind = 'subagent'`, its own
`sessions` row, same schema; filtered out of the picker and dashboard). At the
admission boundary the store intercepts runner results carrying nested
transcripts: the child messages become the subagent session's transcript, and
the parent's tool entry gains a `SubagentRef` pointer in its
`payload.subagent` slot (description, duration, toolset count ride along).
The in-memory `Message` keeps its `children` for the live view; the persisted
entry carries only the pointer. Parent compaction cannot orphan the child;
the child remains independently resumable and auditable.

### 7. Invariants (enforced at the write path)

1. Append-only: `entries`, `entry_memberships`, `projections`, `events` are
   never updated or deleted (GC excepted).
2. Snapshot consistency: materialized tables ≡ replay of events with
   `seq ≤ applied_seq`.
3. Tool pairing: every tool result pairs with an earlier assistant tool call in
   the same session.
4. Checkpoint coherence: every `origin='checkpoint'` entry is referenced by a
   `compact` directive.
5. Late-commit rejection: `(session_id, seq)` primary keys make duplicate or
   stale commits fail.
6. Unknown-kind preservation: loaders keep unrecognized kinds verbatim.
7. `schema_version` bumps only for semantic changes; additive changes do not.

## Alternatives considered

- **Keep the dual-array snapshot and patch its atomicity.** Rejected. The
  defensive machinery exists only to keep two copies of one fact consistent;
  removing the split removes the bug class instead of policing it.
- **Copy-on-fork (duplicate entries per session).** Rejected. Forks would
  either bloat the event ledger with a full transcript copy or rely on an
  out-of-band snapshot mechanism; membership rows make fork an O(1) event.
- **Lineage-walk sharing (Git-style `base_session + up_to_seq`).** Rejected.
  Every read becomes a recursive lineage resolution and directive inheritance
  semantics become ambiguous; the join is cheaper than the walk.
- **Fully flattened entry table with per-kind CHECK matrices.** Rejected. The
  constraint matrix grows quadratically with the kind count and SQL cannot
  express cross-column coherence for open enums.
- **Per-kind tables.** Rejected. Polymorphic foreign keys, per-kind joins, and
  table-per-enum migrations make the open-kind contract expensive.
- **SQL VIEWs as the projection implementation.** Rejected. Two derive
  implementations (SQL + Rust) inevitably drift; SQLite lacks materialized
  views, so every read recomputes.
- **Migrate legacy `sessions.data` snapshots.** Rejected (clean break). The
  transformation (split two arrays into entries + directives, inline nested
  children into subagent sessions, rewrite projection checkpoints as entries +
  directives) is a semantic rewrite with high blast radius for data whose only
  value is resuming old conversations. Legacy sessions are discarded; new
  sessions start on the new schema immediately.

## Consequences

- `SessionData` no longer contains `model_window`, `archived_transcript`, or
  `todos`. The window is `derive(...)` output; archived originals are simply
  entries; projection history is the directives list; the todo list derives
  from the newest `state` entry.
- Prune and compact become append-only directive writes plus (for compact) one
  checkpoint entry. The agent-computed projection result is translated into a
  directive **iff** the resulting view is exactly reproducible (verified by
  wire-normalized comparison); otherwise the transcript rebuilds from the
  caller's window — correctness over cleverness.
- Wire-normalized equality (`Message::to_wire`) decides append-vs-rebuild:
  harness sidecars (timestamps, provenance) are not provider-visible content
  and must never force a rebuild.
- Fork copies memberships, not facts; subagent transcripts live in their own
  sessions; blob deduplication covers shared large payloads.
- Resume restores the exact prior projected view without re-projection.
- Cross-provider sessions keep one factual transcript with per-message
  provenance; boundary transforms handle per-protocol validation (e.g. Gemini
  `thought_signature`) without corrupting stored history.
- **Migration:** `PRAGMA user_version` 5 creates the new tables and drops the
  legacy `messages` table, its FTS structures, and the `sessions.data` snapshot
  payload. Existing sessions do not survive the upgrade; session tooling must
  surface this as "old sessions are retired" rather than attempt best-effort
  conversion.

## References

- ADR-0016 — event-log + snapshot session recovery (ledger role retained).
- ADR-0040 — model-context projection vocabulary (terms retained; dual-array
  storage superseded).
- ADR-0048 — session as single source of truth (retained; SSOT is now the
  event ledger).
- ADR-0091 — durable command ledger (commands remain event/ledger state, not
  transcript entries).
- ADR-0163 — SQLite event ledger and CAS blobs (storage engine retained).
- docs/explanation/agent-design/session-persistence.md — needs a follow-up
  revision to describe the single-transcript model.
