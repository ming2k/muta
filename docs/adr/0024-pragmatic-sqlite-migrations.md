# 0024. Pragmatic SQLite migrations via `PRAGMA user_version`

- **Status:** Superseded — SQLite removed from the project (2026-07)
- **Date:** 2026-06-24

> **Supersession note (2026-07, revised):** The last remaining SQLite consumer
> was `RepeatStore`. It was first rewritten to a standalone `repeat.json`, then
> **folded into session state entirely** — `/repeat` jobs are now session-scoped:
> they live on `SessionData` and persist through the session's event log as
> `SessionEvent::RepeatJobsSet`, alongside the todos, round counter, and
> provider selection. The session that created a job owns it, so resume/fork
> carry the schedule; the scheduler polls the live `SessionStore`. The
> standalone `RepeatStore`, its `repeat.json`, and the `db` migration module
> were all deleted. This removes `rusqlite` (+ its `bundled` SQLite C library)
> from the dependency tree and build. This ADR is retained for history.

## Context

Two stores persist to embedded SQLite databases: `PursuitStore`
(`crates/neenee-core/src/pursuits/store.rs`, table `thread_pursuits`) and
`RepeatStore` (`crates/neenee-core/src/repeat.rs`, table `repeat_jobs`). Both
are opened via `rusqlite` over a local file, wrapped in `Arc<Mutex<Connection>>`
behind `spawn_blocking`.

Neither store used `PRAGMA user_version`. Schema evolution was handled in two
ad-hoc ways:

1. **`thread_pursuits`** carried a `migrate()` that detected an old schema by
   *swallowing* errors:

   ```rust
   let _ = conn.execute("ALTER TABLE thread_goals RENAME TO thread_pursuits", []);
   let _ = conn.execute("ALTER TABLE thread_pursuits RENAME COLUMN goal_id TO pursuit_id", []);
   ```

   The `let _ =` idiom discards every error — a legitimate failure (disk full,
   locked DB, a typo in the statement) is indistinguishable from the "already
   migrated" case. There is no record of *which* migration has run.

2. **`repeat_jobs`** had no migration concept at all — only
   `CREATE TABLE IF NOT EXISTS`, which cannot evolve columns.

This does not scale: there is no version, no audit trail, and failures are
silent.

## Decision

Adopt SQLite's **native** [`PRAGMA user_version`](https://www.sqlite.org/pragma.html#pragma_user_version)
as the schema-version primitive, behind a small shared module
(`crates/neenee-core/src/db/mod.rs`) that the whole workspace reuses.

The module provides three things:

1. **Idempotent probes** — `table_exists` / `column_exists` inspect
   `sqlite_master` and `PRAGMA table_info`, replacing the "try it and swallow
   the error" pattern.
2. **A `Migration` type** — a `{ version, description, apply: fn(&Connection)
   -> Result<...> }` record. Each store owns a `&'static [Migration]` list;
   `fn` pointer (not `Box<dyn Fn>`) keeps it a compile-time constant array.
3. **A `migrate` driver** — reads `user_version`, runs each pending step in
   its **own transaction** (`apply` then `PRAGMA user_version = N`), commits,
   and stops. Already-current databases are a no-op.

Every store entry point — file-backed `open`, async `open_in_memory`, and the
synchronous `open_in_memory_blocking` used by tests — calls `db::migrate`, so
the test path and the production path run the identical migration logic.

No new dependencies are introduced.

## Alternatives considered

- **Flyway / Liquibase** — Java/JVM-based, designed for centrally
  administered server databases (PostgreSQL/MySQL) where a DBA gates releases.
  This project ships a single Rust binary with an embedded SQLite file per
  user; bundling a JVM and an external script directory is a non-starter and
  contradicts ADR-0013's bundled-embed philosophy.

- **`refinery`** — the closest Rust analogue to Flyway. `.sql` files are
  embedded at compile time via `embed_migrations!`. Rejected as
  *premature*: with two tables and single-digit migration steps, the added
  crate and the `.sql`-file workflow buy nothing that the native pragma does
  not already provide. It remains the upgrade path if migration history
  grows large or SQL-file review becomes desirable.

- **`sqlx` (with `sqlx migrate`)** — would require replacing `rusqlite` with
  an async stack and rewriting every store, for no gain: the headline
  feature (compile-time SQL validation against a live database) adds no value
  for DDL statements, which *define* the schema rather than query it.

- **Keep swallowing errors** — rejected: it is the precise hazard this ADR
  removes.

## Consequences

**Positive.** Schema evolution is versioned, auditable, and transactional.
Failures propagate instead of being silenced. The same code serves fresh
databases (which jump straight to the latest version) and legacy ones (which
step through each migration). New schema changes are additive: append a
`Migration` with the next version number.

**Negative.** `PRAGMA user_version` is a single global integer per database
file; it cannot describe *which* of several independent migrations ran if two
stores shared one file. This is a non-issue here because each store owns its
own file, but it would become relevant if stores were ever consolidated into a
shared database (at which point per-table version columns or `refinery`'s
history table would be reconsidered).

**Neutral.** The legacy `token_budget` / `tokens_used` / `time_used_seconds`
columns on `thread_pursuits` are retained by ADR-0010 and are unaffected —
this ADR governs the migration *mechanism*, not the table shape.

## References

- [SQLite `PRAGMA user_version`](https://www.sqlite.org/pragma.html#pragma_user_version)
- ADR-0010 — slim pursuit primitive (legacy columns retained)
- ADR-0013 — skills XDG paths and bundled embed (single-binary ethos)
