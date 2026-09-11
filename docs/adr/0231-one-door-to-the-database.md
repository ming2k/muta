# 0231. One door to the database: a crate-private engine, an actor for writes, a reader for reads

- **Status:** Accepted
- **Date:** 2026-09-11
- **Builds on:** ADR-0163 (unified SQLite ledger & CAS), ADR-0168 (complete SQLite unification), ADR-0178 (persistent single-writer actor), ADR-0187 (incremental append & blob reference ledger), ADR-0196 (supervised writer, typed errors)

## Context

ADR-0178 declared the single-writer actor and retired open-write-drop from the
*session* path. ADR-0196 made the actor survivable and its errors typed. Both
are about the actor's *internals*. Neither said anything about who may open a
connection, and the answer across the tree was: anyone.

When this ADR was written, `DatabaseEngine::open` was called from **35** call
sites across **13** files outside `db.rs`, in two crates, for both reads and
writes (the table below lists the distinct locations):

| Site | Kind | Why it was wrong |
|---|---|---|
| `usage_stats.rs` (`persist_day`, `prune_old_days`, `list_days`, `read_day`) — 5 opens | **write** | Read-modify-wrote a whole day blob to `muta.db` on a fresh connection, off the actor. |
| `workspace_security.rs` (`persist`, `read_state`) | **write** | Trust state written around the actor; also a migration write inside its own read path. |
| `connection_usage.rs` (`save`, `save_exact`, `load`) — 3 opens | **write** | Merged-then-wrote a whole blob around the actor. |
| `route_settings.rs` (`save`, `read_file`) | **write** | Same shape, plus a migration write inside a read. |
| `session/mod.rs` (`persist_to`) — 3 opens | **write** | Wrote `save_session_full` directly whenever the pinned path was not the default `muta.db`. |
| `session/mod.rs` (`load_or_seed`) | write | Wrote the legacy path alias through its own connection. |
| `task_ledger.rs` (`record_outcome`, `prune`, `rehost_all`) | **write** | The durable task ledger wrote `kv_store` rows around the actor. |
| `handlers_history.rs` (`save_input_history`, `delete_input_history_entry`) — 4 opens | **write** | The daemon's own history front door wrote around the actor. |
| `registry.rs` (`collect_entry_garbage`) — 3 opens | **write** | Maintenance DELETEs ran on a fresh connection. |
| `session/store.rs` (`list`, `detail`, `resolve_session`) — 3 opens | read | A second reader per call, uncoordinated with policy. |
| `session/history.rs` (2), `session/mod.rs` (`run_doctor`), `archivist.rs` (4), `registry.rs` (lookups), `background_jobs.rs`, `session/tests.rs` | read | Same. |

Every write in that table is a **second writer** to one SQLite file. The
consequence is not theoretical. On the reporting workstation, one day of the
daemon's log carried **1474** occurrences of:

```
WARN muta_persistence::usage_stats: could not persist usage-stat record
  error=could not persist usage day to sqlite: database is locked
```

Reproduced locally against a copy of the same database: a writer holding the
transaction lock for 2 s blocked the usage write for **2.03 s** (it succeeded,
late); holding it for 8 s blocked for exactly **5.010 s** and then failed —
`busy_timeout`, `db.rs:61` — and the usage record was *dropped*, because
`UsageStatSink::record_usage` logs and swallows. The retry-on-`WriterDown`
policy of ADR-0196 D3 protects the actor's clients; nothing protects a client
that is not using the actor.

Two more facts made this an architecture problem rather than a bug list:

1. **The measurement was invisible.** Every one of those writes burned up to
   5 s of the round's tail. The activity bar renders that whole tail as
   `finalizing response`, so the symptom surfaced as "the TUI hangs at the end
   of a round" — the diagnosis (concurrent writers) was nowhere near the
   symptom (UI latency after a stream ends).
2. **Nothing prevented the next one.** `DatabaseEngine` was `pub` with `pub`
   methods (`open`, `connection`, `connection_mut`, `set_kv`, …), so writing
   the fifteenth bypass was as easy as writing the first — and the two that
   mattered were added by ordinary feature work (ADR-0122's usage mirror,
   ADR-0190's task ledger).

## Decision

**Outside `muta-persistence::db`, no module may name a SQLite connection.** The
type is the constraint: `DatabaseEngine` is `pub(crate)`, its methods are
`pub(crate)`, and the shipped surface exposes exactly two ways in.

### D1. Every mutation goes through the single-writer actor

`PersistenceHandle` (globally reachable as `get_persistence_handle()`, or
`PersistenceHandle::spawn(path, blobs)` for a store pinned to its own
database) is the only way to change durable state. Every synchronous verb goes
through **one** bridge, `PersistenceHandle::run_blocking`, which enqueues the
command and waits for its ack. Six hand-rolled copies of that bridge are gone
(three with their own `block_in_place` dance, three that called
`blocking_send`/`blocking_recv` directly and were therefore a panic waiting for
an async caller).

The bridge is **runtime-flavor aware**, and the reasoning is worth recording
because getting it wrong has two opposite failure modes:

- On a **multi-thread runtime**, `block_in_place` is used: it parks the worker
  and hands its core back, so the runtime keeps progressing and no thread is
  spawned. (The `blocking_send`/`blocking_recv` pair is *only* legal here
  because `block_in_place` exits the runtime context while the closure runs.)
- On a **current-thread runtime**, or with no runtime at all, the wait moves
  off this thread: `spawn` + `join`. Calling `blocking_send` directly on a
  current-thread worker instead panics with *"Cannot block the current thread
  from within a runtime"* — which is exactly what the three
  direct-calling verbs did.

This is a **coupling**, not a local choice: the `spawn` + `join` branch is
deadlock-free only because D3 guarantees the supervisor owns a thread of its
own. The two decisions have to move together, so both are pinned by tests
(`one_door.rs`, four runtime flavors including single-worker).

### D2. Every read goes through the reader the handle hands out

`DbReader` wraps a connection snapshot and exposes a **read allow-list**
(`get_session`, `list_sessions`, `list_session_summaries`, `latest_session`,
`lookup_session_workspace`, `resolve_session_prefix`, `get_session_detail`,
`read_session_transcript`, `live_blob_hashes`, `search_history`,
`search_history_relaxed`, `load_input_history`, `load_request_projections`,
`get_kv`, `get_json`, `list_kv_keys_with_prefix`). The only constructor is
`PersistenceHandle::reader()`; there is no public `DbReader::open(path)`.

Reading through the handle — not through the actor — is deliberate: WAL readers
neither block the writer nor are blocked by it, so a read must never queue
behind a slow write. `DbReader` also gives one place to change read policy
later (a pooled reader, a read-only URI) without touching call sites.

### D3. The actor must own progress

`PersistenceHandle::spawn` hosts the supervisor on a multi-thread runtime's
worker **or on a thread of its own** — never on a current-thread runtime that a
caller can block.

The pre-existing bridge took the thread path for any `!MultiThread` caller but
left the supervisor as a task on that same current-thread runtime. Under
`#[tokio::test]` (which is current-thread by default) `handle.supervisor
.blocking_send(..)` then `join`-ed a thread while the only thread that could
ever serve the ack sat parked in `join`: every synchronous verb deadlocked.
Adding the actor to `SessionStore`'s construction path turned that latent trap
into 16 hanging tests, which is how it was found. `spawn` now refuses to host on
a current-thread runtime, and the invariant is asserted from four flavors.

### D4. The rule is mechanized, not merely documented

`crates/muta-persistence/tests/one_door.rs` scans the tree's Rust sources and
fails on any of `DatabaseEngine`, `Connection::open`, `initialize_db`,
`rusqlite::Connection` outside `db.rs` and the guard itself. The compile-time
half (privacy) covers the *type*; the scan covers the *library*, since
`rusqlite` remains an ordinary dependency and a bypass needs no engine at all.

The door's own shape is asserted too (`pub(crate) struct DatabaseEngine`,
`pub struct PersistenceHandle`, `pub fn reader`, `pub struct DbReader`,
`get_persistence_handle`), so deleting a door fails a test rather than silently
leaving a crate with no way to persist.

## Alternatives considered

**Keep the engine public, add a lint / review checklist.** Rejected: the rule
is about *where* handles may be created, which is not a type property, and the
two bypasses that caused the incident were both reasonable-looking feature
code. A rule a reviewer must remember is a rule that decays; a rule the
compiler and a test enforce does not.

**Make every read go through the actor too.** Rejected: it serializes reads
behind writes for no durability benefit. WAL exists precisely so readers do not
wait on the writer. (The Archivist's history search and the report sweep both
run off the writer's critical path today.)

**Give `DbReader` a public `open(path)`.** Rejected: it re-creates the exact
hole this ADR closes — a caller-chosen second connection. The reader is bound
to the handle's database; a store that needs a different database spawns a
handle for it (`WorkspaceSecurityStore::for_path`, `SessionStore::for_path`,
`UsageStatsStore::with_root`), which keeps the write path inside the actor.

**Pool reader connections.** Deferred, not rejected: short-lived readers are
what the previous code did, and the sweep that read 8 day files now pays for
exactly one reader instead of 8. A pool is a change to `reader()` alone.

**Fix only `usage_stats` (the observed offender).** Rejected: it would leave
twelve other bypasses, including three more whole-blob read-modify-write stores
with identical failure modes, and no force preventing the next one.

## Consequences

### Positive

- **One writer, structurally.** `database is locked` from a *dropped* record
  becomes unreachable: every mutation is enqueued on one channel and executed
  by one thread.
- **A read cannot wedge the writer, and the writer cannot wedge a read.**
- **Latency, but only the lock-shaped part of it.** The usage-day sweep opens one
  reader instead of one per day file, and no round-tail write can lose 5 s to a
  lock it is about to be denied. Writes that used to race now serialize behind
  the actor's ack, which is the honest cost of one writer. **This ADR does not
  shorten the round tail by itself** — see the scope note below.
- **Discoverability:** adding persistence reach is an edit in `db.rs` plus a
  door in `DbReader`/`PersistenceHandle` — a reviewable act, not a side effect.
- **Dead code deleted rather than documented:** the raw `connection()` /
  `connection_mut()` accessors, the second `open_in_memory` constructor, the
  engine-level `set_json`/`save_session_full` writers, and six blocking bridges
  (three `block_in_place` dances, three direct `blocking_send`/`blocking_recv`
  pairs) are gone. The two constructors tests genuinely need
  (`open_in_memory`, `save_session_full`) are `#[cfg(test)]`.

### Scope: what this ADR deliberately does not fix

The symptom that motivated the investigation was the activity bar sitting on
`finalizing response` for seconds after a stream had ended. The one-door rule
removes the **lock-related** part of that stall and the silent record loss
behind it. It does **not**, on its own, move work off the round boundary — and
the dominant term was never a database operation:

- `UsageStatsStore::report(200)` ran **inline** on the round path, twice:
  inside `execute_round` between `RoundCompleted` and its return (so it gated
  the tail's idle snapshot, i.e. the bar) and again in the tail. Cost:
  ~85–110 ms release / ~410–430 ms debug per call, dominated by deserializing
  ~25 000 records across the day blobs.
- The same boundary also builds `TokenUsageReport`, which the TUI must
  deserialize before it can apply the idle snapshot.

That second, separate fix has since landed: the cross-session aggregate is now
fetched on demand when the `/usage` overlay opens and is computed on the
blocking pool, with `TokenUsageReport` remaining a (cheap, in-memory)
round-boundary push. A controlled A/B on a real corpus puts the bar's dwell time
at **434/474/435 ms → 35.6/14.7/13.1 ms** (debug) and **122.5/97.9 ms →
2.7/2.9/9.3 ms** (release). See ADR-0209's 2026-09-11 addendum for
the decision record, including the part of the notification-driven contract it
narrows.

It is recorded here rather than folded into this ADR's decision because the two
changes are independent: the one-door rule is what made the second one
*possible* (the aggregate became an on-demand read off the handle, rather than a
bypass), but neither implies the other.

### Negative / neutral

- `UsageStatsStore`, `WorkspaceSecurityStore`, `ConnectionUsage`,
  `RouteSettingsStore` and `TaskLedger` hold a handle. A store pinned to a
  non-default database owns its actor (one thread) rather than opening a
  connection per call — cheaper in the steady state, but it does mean a
  forgotten handle keeps a thread alive until dropped.
- **`run_blocking` and `spawn` carry a joint invariant** : the `spawn + join`
  branch is safe only while `spawn` keeps the supervisor off the caller's
  current-thread runtime. Neither half may be "simplified" alone. Pinned by
  `one_door.rs`'s four flavor tests; documented in both function comments.
- Sync verbs cost one thread spawn on a current-thread or runtime-less caller,
  and a parked worker (`block_in_place`) on a multi-thread one. They are
  configuration, maintenance, ledger and history writes — never the model hot
  path.
- `SessionStore::for_path` now spawns its handle before loading, so `load_or_seed`
  takes both a reader and a writer. Three call sites pass both.
- The source scan is a heuristic: it catches the four ways a connection can be
  created, not an exotic one (e.g. `rusqlite::ffi`) that nobody writes. It is a
  tripwire for the plausible mistake, not a proof.
- The archive/registry lookups still open a reader per call; a pooled reader is
  the obvious follow-up if a hot read path appears.
- `rusqlite` remains a dependency of `muta-persistence` only, so the compile-time
  half of the rule additionally holds *for the rest of the workspace* today. If a
  second crate ever needs it, the scan is what keeps the rule honest.

## References

- ADR-0014: XDG persistence architecture
- ADR-0048: session as the single source of truth (the turn-persist save point)
- ADR-0122: durable cross-session usage statistics (the `UsageStatSink`)
- ADR-0163: unified SQLite event-sourced ledger and CAS
- ADR-0168: complete SQLite unification and legacy persistence purge
- ADR-0178: persistent single-writer actor and zero-churn WAL architecture
- ADR-0187: persistence v2 incremental append and blob reference ledger
- ADR-0196: supervised persistence writer and typed persistence errors
- ADR-0218: durable request-projection archive
- `crates/muta-persistence/tests/one_door.rs`: the mechanized rule (one-door
  scan + door-shape assertions + the four runtime-flavor sync-verb tests)
