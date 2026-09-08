# 0187. Persistence v2: Incremental Append, Blob Reference Ledger, and Honest Compatibility

- Status: Accepted
- Date: 2026-09-07
- Deciders: ming
- Consulted: —
- Informed: —

## Context and Problem Statement

ADR-0186 rebuilt session persistence around the single-transcript model
(entries + projection directives in SQLite), but its implementation drifted
from its contract in ways that matter for durability and cost:

1. **The event ledger is dead code.** The `events` table, `append_event`, and
   the `applied_seq` high-water mark are written by nothing and replayed by
   nothing, while the ADR claims events are the source of truth.
2. **Every save is a full rewrite.** `save_session_full` deletes and
   re-inserts every membership and projection row on each turn commit —
   O(N) SQL per turn, O(N²) cumulative over a session.
3. **Blob GC is scoped to a dead file layout.** The garbage collector marks
   live blobs by scanning `projects/*/sessions/*.json`, files that are no
   longer written. A maintenance pass would classify every blob as garbage
   and delete the bodies of every offloaded entry (>4 KB) — a scheduled
   data-loss bug.
4. **Unknown payloads are silently dropped.** The contract says readers
   preserve unknown entry kinds verbatim; the loader instead skips them, and
   the next save orphans them permanently.
5. **Working state loses fields.** `SessionTree` and `digest_anchor` are
   never persisted; resume silently disables digest auto-refresh and drops
   the fork DAG.
6. **The written checksum is never verified**, and the session row's
   `_ms`-suffixed timestamp columns store seconds.
7. **Full-text search is silently broken**: the entry-insert trigger fires
   before the membership row exists, so `fts_entries` stays empty.
8. **Session listing lost its indexes** in migration 5 (the rebuild dropped
   them without recreating).

## Decision Drivers

- Durability: a save path that can lose data under any supported sequence is
  unacceptable.
- Cost: persistence work per turn must be proportional to the delta, not the
  session.
- Honesty: code, schema, and documentation must not disagree. A promise the
  implementation does not keep must be removed or implemented.
- Forward compatibility: an older binary must never corrupt or orphan data
  written by a newer one.

## Considered Options

- Option 1: Implement true event sourcing (write events, replay on load).
- Option 2: Keep the materialized-tables model, make saves incremental, and
  delete the event-ledger fiction.
- Option 3: Keep full rewrites, fix only the GC.

## Decision Outcome

Chosen option: "**Option 2**", because the materialized tables already are
the durable ledger (append-only memberships keyed by a gap-free seq
watermark, enforced by the primary key), and replay would add a second
representation of the same facts with no reader. The seq watermark plus a
transcript **generation id** gives incremental saves with self-healing
consistency.

### Positive Consequences

- Turn commits write O(delta) rows instead of the whole transcript.
- Blob liveness is decided by a `blob_refs` table maintained transactionally
  with the saves that introduce the references; GC cannot delete a live blob.
- Unknown entry payloads and directive payloads are preserved verbatim
  (raw JSON rows) across load/save by the current binary — older binaries
  become safe around newer data.
- `SessionTree` and `digest_anchor` persist; resume reproduces the fork DAG
  and digest refresh works across restarts.
- The session row's checksum is redefined as a working-state checksum and is
  actually verified on load (legacy rows are exempted via `schema_version`).
- FTS is fixed (membership-driven triggers + backfill) and session listing
  indexes are restored.
- The dead event ledger, `session_blobs` table, and `applied_seq` are
  removed; the schema no longer promises what the code does not do.

### Negative Consequences

- Schema version 6 drops the unused `events`/`session_blobs` tables and the
  `applied_seq` column — all write-dead, so nothing is lost.
- Incremental saves keep legacy inline entry bodies in place (offload only
  applies to newly inserted rows); the blob-ified clone and the inline row
  remain consistent because loads rehydrate from `content_blob` regardless.
- Entries orphaned by session deletion still accumulate (facts are shared by
  identity across forks); an entry-level GC is future work and must respect
  fork sharing.

## Pros and Cons of the Options

### Option 1 — Event sourcing

- Good, because replay is a single audit mechanism.
- Bad, because it duplicates the transcript as a second source of truth and
  the workload (resume speed) pays for a replay path nobody uses.

### Option 2 — Incremental materialized tables (chosen)

- Good, because the primary key already enforces append-only seq integrity.
- Good, because generation-id escalation makes any divergence fall back to a
  full rewrite in the same transaction — correctness without a flag protocol.
- Bad, because "events" vocabulary in ADR-0186 must be reinterpreted; this
  ADR records that reinterpretation.

### Option 3 — Fix GC only

- Good, because it is small.
- Bad, because it leaves the O(N²) write path and the silent data-loss load
  path in place.

## Links

- Supersedes the event-ledger claims of [ADR-0186](0186-single-transcript-projection-directives-persistence.md) (the single-transcript model itself stands).
- Related: [ADR-0168](0168-complete-sqlite-unification-and-legacy-persistence-purge.md) (SQLite SSOT).
