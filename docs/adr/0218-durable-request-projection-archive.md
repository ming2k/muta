# 0218. Durable Request-Projection Archive

- Status: Accepted
- Date: 2026-09-10
- Scope: session persistence, observability, model request composition
- Deciders: Maintainer
- Consulted: Maintainer–assistant design discussion
- Informed: Agent, persistence, and provider integration maintainers
- Depends on: [ADR-0213](0213-model-request-composition-and-context-lifecycle.md), [ADR-0217](0217-request-components-and-derived-cache-plan.md)
- Amends: [ADR-0186](0186-single-transcript-projection-directives-persistence.md), storage surface only (a new non-transcript projection table)

## Context and Problem Statement

ADR-0213 makes the request-local temporary context `E_n` explicit and forbids
promoting it into the interaction record or later model history. That protects
history fidelity and prefix reuse, but it also means the bytes the model saw in
`E_n` — and the identity of the cacheable prefix at that moment — leave no
durable trace. Debugging, review, and cache-prefix explanation all need the
request scene after the process exits.

Writing `E_n` into the transcript is the anti-pattern ADR-0213 rejects.
Archiving it as a `sessions` JSON column is a second, subtler mistake: the
projection is produced on the request hot path, and a row column forces an
O(records) serialization on every session save, is carried in `SessionData`
memory, checksum, and fork copies, and is written through a full-session persist
per request. The repository already retired exactly that shape for the usage
ledger (migration 8).

The request projection is a distinct data surface (ADR-0213). It deserves its
own key-addressed storage, an off-hot-path write, and on-demand reads — never a
transcript, and never session state.

## Decision Drivers

- Reconstruct the exact request scene after the fact (temporary payload, prefix
  identity, capture time).
- Keep the archive strictly out of history, out of `SessionData`, and out of
  replay.
- Make the write O(1) and non-blocking on the request hot path.
- Bound retention and memory so a long session cannot grow it without limit.
- Reuse the established key-addressed ledger pattern rather than inventing a new
  engine.

## Considered Options

1. Do not persist `E_n`.
2. Persist `E_n` into the transcript / model history.
3. Persist a dedicated request-projection record in a key-addressed,
   non-transcript table, written off the hot path and read on demand.

## Decision Outcome

Choose option 3.

### The Record

`muta_contracts::RequestProjection` captures one assembled request:

| Field | Meaning |
| :--- | :--- |
| `round` / `turn` | Logical coordinates (the storage key) |
| `created_at_ms` | Capture time |
| `prefix_fingerprint` | Deterministic identity of the cacheable prefix `S | H | I` (ADR-0217) |
| `conversation_messages` | Count of `S | H | I` messages |
| `temporary_context_tokens` | Token volume of `E_n` |
| `temporary_context` | `E_n` captured verbatim (bounded by the producer budget) |

### Storage

A dedicated table (DB schema v11), not a `sessions` column:

```sql
CREATE TABLE request_projections (
    session_id    TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    round         INTEGER NOT NULL,
    turn          INTEGER NOT NULL,
    created_at_ms INTEGER NOT NULL,
    payload       TEXT NOT NULL,
    PRIMARY KEY (session_id, round, turn)
);
```

The record body is the `payload` JSON. The primary key makes the write idempotent
per logical invocation. `ON DELETE CASCADE` means the archive dies with the
session, and — because it is not part of `SessionData` — it is never serialized
into a session save, never checksummed, and never copied into a fork.

### Write Path

- The agent fires a synchronous, infallible sink once per freshly assembled
  request. A transport retry reuses the prepared snapshot and does not re-fire.
- The session driver installs the sink; subagents, the review diagnostic, and
  tests leave it unset.
- The sink enqueues a single `RecordRequestProjection` command through the
  single-writer actor with `try_send` (fire-and-forget). Dispatch never waits on
  the store lock, a session clone, or disk. A dropped write is logged.
- The writer performs one `INSERT OR REPLACE` and then trims the session's rows
  to the newest `MAX_RETAINED_REQUEST_PROJECTIONS` (64). No other session row is
  touched.
- Callers that need the durable write confirmed (tests, tooling) use the awaited
  `record_request_projection`.

### Read Path

`SessionStore::request_projections()` opens the store and loads the session's
rows (oldest first, capped) on demand, on a blocking thread. It is never called
by the window/transcript/compaction/replay paths and never populates
`SessionData`.

### Invariants & Behavioral Boundaries

1. The archive is not history: it never appears in `transcript`, `SessionData`,
   the model window, compaction, or any replay path.
2. It is never a basis for a later request.
3. The hot path never blocks on it: the sink is synchronous and only enqueues.
4. Retention is bounded by a per-session ring; memory is bounded because reads
   are capped and nothing is held in `SessionData`.
5. One logical invocation yields at most one row (primary key).
6. The record is evidence, not a promise: the fingerprint and counters are
   derived, and the record must not assert a provider cache hit.
7. The archive inherits the session store's privacy posture; it is not given a
   separate redaction policy, because `E_n` is already sent to the provider and
   the rest of the store persists equally sensitive material.

### Positive Consequences

- The exact request scene is reconstructible after the process exits.
- The write is O(1) and off the request's correctness path.
- The archive cannot contaminate history, `SessionData`, forks, or replay.
- Storage and memory stay bounded by retention and the producer budget.

### Negative Consequences & Trade-offs

- Stored `E_n` may contain workspace paths or other sensitive context; it must
  be treated with the session file's sensitivity.
- The ring is lossy: very old projections are unavailable.
- Rows are pruned by capture order; two requests captured at the same timestamp
  are ordered by insert order as a tiebreak.

## Rejected Alternatives & Negative Knowledge

### Do Not Persist (Option 1)

- Why considered: zero storage and zero privacy surface.
- Why rejected: no way to reconstruct what the model saw or why a prefix broke.

### Persist Into History (Option 2)

- Why considered: simplest; reuses the transcript writer.
- Why rejected: ADR-0213 already rejects "persist every dynamic version" —
  context-window bloat, stale conflicting evidence, and broken prefix reuse.

### Persist as a `sessions` JSON Column

- Why considered: reuses `round_interrupts`/`commands` and the existing save
  path; simple.
- Why rejected: the projection is written per request, and a row column forces
  O(records) serialization on every save, lives in `SessionData` memory,
  participates in the session checksum, and rides into forks. This is the exact
  shape migration 8 retired for the usage ledger. The write must be O(1) and the
  data must not be session state.

### Make the Archive Authoritative / Replayable

- Why considered: one path for reconstruction and resume.
- Why rejected: conflates the request-projection surface with the interaction
  record. The acceptance criteria require replay to be unable to read it.

## Relationship to Existing Decisions

ADR-0213 owns the three data surfaces and the `E_n` lifecycle. ADR-0217 owns the
cacheable prefix identity this record embeds. ADR-0186 established non-transcript
projection state; this record uses a dedicated table rather than extending the
session row. Provider breakpoint placement remains a provider-local policy over
the encoded body; the `CachePlan` supplies the provider-neutral prefix identity
used for the archive and diagnostics.

## Verification and Adoption

- A real `execute_round` archives exactly one projection and the window excludes
  `E_n` (`execute_round_archives_a_request_projection_outside_the_window`).
- Recording leaves `model_window`/`transcript` unchanged across reload; the
  record round-trips (`request_projections_persist_outside_the_window`).
- Re-recording the same `(round, turn)` replaces rather than duplicates the row.
- Retention evicts rows beyond the cap
  (`request_projections_are_retention_bounded`).
- The sink fires once and never blocks or fails dispatch
  (`request_projection_sink_fires_and_is_non_fatal`).
- Database schema advances to v11 with an idempotent table creation, and the
  migration catalog fingerprint is updated in lockstep.

## Links

- [ADR-0213: Model request composition and context lifecycle](0213-model-request-composition-and-context-lifecycle.md)
- [ADR-0217: Request components and a derived wire cache plan](0217-request-components-and-derived-cache-plan.md)
- [ADR-0186: Single-transcript persistence with projection directives](0186-single-transcript-projection-directives-persistence.md)
