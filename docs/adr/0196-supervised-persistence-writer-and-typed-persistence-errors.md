# 0196. The persistence writer is supervised: respawn on death, typed errors, health on the wire

- **Status:** Accepted
- **Date:** 2026-09-08

> **Implementation addendum (2026-09-08, landing notes).** Landed as specified,
> with two honest narrowings, both recorded per D6's discipline:
>
> 1. **A fifth variant, `Encode`, was added.** A value that fails to encode
>    before reaching the writer (JSON serialization) is a caller-data defect,
>    not a SQL fact and not a lifecycle fact; pretending it was `Engine`
>    would lie. Every variant is exercised by the fault-injection suite
>    (`db.rs` `tests::supervision`), including the test-only `Die` command
>    that simulates actor death to drive the respawn/health-restore path.
> 2. **`SessionStore`'s error boundary stays `String` for now; the typed
>    error owns the writer layer.** `PersistenceHandle` and the actor ack
>    surface are fully typed, `persist_with_usage` / `persist_full_rewrite`
>    classify `WriterDown` for the D3 retry (5 attempts, ~3.1 s, idempotent
>    by ADR-0187's watermark), and the boundary string is now *assembled
>    from* the typed Display. Threading `PersistenceError` through
>    `commit_turn` → `TurnPersistFn` → `HarnessError` rewrites the harness
>    error taxonomy — that belongs with the harness's own error ADR, not
>    this one.
>
> One deliberate clarification: D4's per-failure live signal for
> out-of-round sites is carried by the *health transitions* themselves — a
> writer that dies while the user is idle publishes `Recovering`/`Down`
> immediately, and every frontend banner is live. Per-command failure
> notices would add a second signal for the same fact.

## Context

Durability rests on a single-writer actor (ADR-0178/0187): one dedicated
thread (`muta-persistence-writer`, `crates/muta-persistence/src/db.rs:2580`)
owns the SQLite engine; every caller talks to it through the clonable
`PersistenceHandle` over a bounded mpsc + oneshot ack.

The actor has no supervisor, and its death is both permanent and silent:

1. **Death at birth.** If `DatabaseEngine::open` fails on the writer thread
   (DB locked by another process, schema migration failure, permission, disk
   full, corruption), the thread logs one `error!` and returns
   (`db.rs:2588-2594`), dropping its `rx`. Every later command send fails
   immediately.
2. **Death by panic.** `engine.save_session_inner(...)` runs unwrapped inside
   the loop; a panic unwinds the thread and drops `rx` the same way.
3. **The death is cached forever.** `GLOBAL_HANDLE` is a `OnceLock`
   (`db.rs:2563-2575`). A handle whose writer is dead is cloned for the rest
   of the process lifetime; there is no respawn, no health query, no way to
   distinguish "one write failed" from "durability ended 20 minutes ago".
4. **The failure is type-smuggled.** `save_session` maps channel errors into
   `rusqlite::Error::ToSqlConversionFailure(Box<SendError/RecvError>)`
   (`db.rs:2685, 2688`) — Display: "channel closed". `SessionStore` wraps it
   into a string (`session/store.rs:353-356, 364-367`), the harness into
   `HarnessError::Other` (`agent/state.rs:639-640`), and the TUI renders the
   composite string as an inline transcript notice
   (`apps/tui/crates/mutx/src/lib.rs:1857-1899`). No layer can classify,
   retry, or escalate on what actually happened.
5. **Most failures are invisible.** Persist calls outside the round
   boundary — todos (`orchestration.rs:1497`), round-interrupt records
   (916), retry-point cleanup (1037), digest catch-up (1120), usage upserts
   (1299), prune commits (538) — are `tracing::warn!`-ed and never reach any
   frontend.

Observed incident: a user submits a prompt and instantly sees
`session persist task failed: channel closed`. The turn-persist save point
(`fire_turn_persist` → `commit_turn` → `persist_with_usage`) is the first
caller after the writer died, so the failure surfaces at the worst possible
moment and reads as a corruption of the user's action, when it is a
process-lifecycle defect that predates the turn.

The diagnosis is architectural, not a missing retry: **the one component
whose job is durability is the only component with no failure story.** A
patch (log louder, retry once) leaves the dead-handle cache, the string
errors, and the warn-only coverage gap intact. Break clean.

## Decision

The writer is a supervised long-lived service with a typed contract and a
user-visible health state. Four sub-decisions:

### D1. The actor is supervised

A supervisor owns the actor thread's lifecycle:

- On actor-thread exit — engine-open failure **or** panic — the supervisor
  respawns it with bounded exponential backoff (default: 100 ms → 5 s cap,
  reset on a healthy ack).
- Per-command execution is wrapped in `catch_unwind`, so one poisoned
  command (a malformed row, a constraint-triggering panic path) settles that
  command's ack as `Err` instead of killing the writer.
- Health is a first-class, queryable state on the handle:
  `Healthy | Recovering { attempt, since } | Down { error, since }`.
  `Healthy` is restored by the first successful command, not by spawn alone.
- The `OnceLock` caches the **supervisor** handle, never a bare channel to a
  possibly-dead thread. A respawned writer is reachable through the same
  handle; no call site changes.

### D2. Persistence errors are typed

A `PersistenceError` enum replaces every `ToSqlConversionFailure` smuggling
in `PersistenceHandle` methods (`save_session`, `save_session_blocking`,
`upsert_session`, `delete_session`, `rename_session`, KV and history
commands — the whole `db.rs:2668-2960` surface):

- `WriterDown` — the send or the ack failed because the actor is not
  serving. This is a *lifecycle* fact, not a SQL fact.
- `Engine(source)` — the actor served the command and SQLite rejected it.
- `Poisoned` — the command panicked the actor's handler; the actor survived.
- `Closed` — the handle is shut down (explicit shutdown only; today there is
  none, and shutdown becomes a deliberate daemon-lifecycle verb, not an
  accident).

`SessionStore` stops owning error prose: `persist_with_usage` returns
`Result<(), PersistenceError>`; the "session persist task failed:" string is
assembled at the surface that shows it, from the variant, not baked at the
store.

### D3. Retry where idempotency already exists

ADR-0187's delta append escalates by transcript generation, so a retried
`save_session` is idempotent. Callers that must not lose a turn
(`persist_with_usage`, `persist_full_rewrite`) therefore retry `WriterDown`
a bounded number of times across the supervisor's backoff window before
failing; a mid-round persist failure remains a deliberate round-ending error
(unchanged semantics, now with a truthful cause). One-shot KV/history
commands do not retry; their acks resolve `Err(WriterDown)` and the caller
decides.

### D4. Health is on the wire

- Health transitions are published additively (ADR-0134) as a monitor/notice
  event (`PersistenceHealth { state, detail }`), emitted by the supervisor.
- Every frontend renders degradation: the TUI shows a persistent degraded
  banner while `Recovering`/`Down` (a banner, not a transcript notice — the
  condition outlives any message), and the dashboard/`muta status` surfaces
  it.
- The warn-only call sites (D-decision 5 above) keep their `warn!` **and**
  route through the same live signal, closing the coverage gap: a failed
  todo-persist during an idle session is visible, not just logged.

### D5. No half-durable illusions

While `Down`, pending commands are drained with `Err(WriterDown)` acks —
they were never durable, and pretending otherwise (buffering them for a
writer that may never return) converts a visible failure into silent data
loss. The bounded channel keeps its 1024 admission bound; `WriterDown`
failures free capacity immediately instead of backing up turns.

### D6. Contract discipline

Every `PersistenceError` variant is exercised by test: engine-open failure
injection (bad path/permissions), panic injection in a command handler,
respawn-and-recover, health transitions, and drain-while-down. The review
gate from ADR-0190 D7 applies: a persistence call site that ignores its
`Result` is a defect, and a check is added for the `let _ = …persist…`
pattern.

## Alternatives considered

- **Open the engine per call (no actor):** loses the single-writer batching,
  WAL discipline, and blob-store ownership of ADR-0178; invites
  multi-writer races. Rejected — the actor is right, its supervision was
  missing.
- **Panic the process when the writer dies:** makes the failure loud but
  kills a live interactive session (and every other session on the daemon)
  for a background-durability defect; availability regresses. Rejected.
- **Watchdog that detects and exits gracefully:** same outcome with extra
  machinery; the user's session still dies. Rejected.
- **Buffer commands while down and replay on respawn:** hides loss-window
  semantics behind a queue; callers' acks would lie about durability. The
  idempotent-retry of D3 gives the same recovery where it matters, with
  honest acks. Rejected.
- **Map channel errors to a dedicated `rusqlite` error code:** keeps the
  lie that these are SQL errors and still hides lifecycle state. Rejected —
  the type must not be `rusqlite::Error` at all.

## Consequences

### Positive

- The "channel closed" class ends: the observed incident becomes a
  transient `Recovering` banner, and the next prompt round-trips against a
  respawned writer.
- Error handling becomes classifiable at every layer; `HarnessError` can
  distinguish "your message was not saved" from "the database rejected the
  row".
- Durability degradation is user-visible everywhere, including outside
  rounds.
- The writer gains shutdown semantics for free (D2 `Closed`), which the
  daemon lifecycle currently lacks.

### Negative / trade-offs

- A supervised respawn can mask a persistently broken DB (e.g. corruption)
  behind recurring banners instead of a hard failure; the backoff cap and
  the `Down { error }` detail keep the cause in the user's face. Accepted —
  a degraded session the user can still read beats a dead one.
- `catch_unwind` around commands is unusual outside FFI and must stay
  surgical (per command, `AssertUnwindSafe` audited). The alternative —
  treat any panic as fatal to durability — is exactly today's bug.
- Typed errors touch every `PersistenceHandle` call site once; the compiler
  walks the list.

### Implementation milestones

1. **M1 — Typed errors.** `PersistenceError` across the handle surface;
   `ToSqlConversionFailure` smuggling deleted; `SessionStore` and harness
   mapping updated.
2. **M2 — Supervision.** Supervisor loop, backoff respawn,
   `catch_unwind` per command, health state on the handle; `OnceLock` caches
   the supervisor.
3. **M3 — Health on the wire + call-site retries.** Notice/monitor event,
   TUI banner, dashboard row; D3 retry policies; warn-only call sites emit;
   fault-injection tests.

## References

- ADR-0178 (persistent single-writer actor and zero-churn WAL), ADR-0187
  (persistence v2, watermark-idempotent delta append — the retry license),
  ADR-0190 (D7 contract discipline; supervised task fabric as the house
  supervision idiom), ADR-0134 (additive wire compatibility), ADR-0193
  (round-EOL digest single-flight — another persist caller that benefits
  from D3).
- Prior art: Erlang/OTP supervisors (`one_for_one`, backoff via restart
  intensity); systemd `Restart=` with `RestartSec`; SQLite's own guidance
  on retryable (`SQLITE_BUSY`) vs fatal errors — the enum mirrors that
  split.
