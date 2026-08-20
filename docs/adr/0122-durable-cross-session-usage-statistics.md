# 0122. Durable cross-session usage statistics

- **Status:** Accepted
- **Date:** 2026-08-20

## Context

neenee already accounts every provider request attempt. The
`TokenSourceLedger` keys terminal attempts by `(session, actor, round, turn,
attempt)`, splits reported vs. estimated tokens, and — via
`persist_request_usage` — mirrors those records into the session file as
`request_usage_records`. The Context Usage modal renders them per session.

That accounting is *session-scoped by construction*, which makes it useless
for the questions users actually ask about their own habits:

- *"How many tokens did I burn today? This week?"* — no view aggregates
  across sessions.
- *"Which models do I actually use, and how much?"* — per-session rows only.
- *"What did I use last Tuesday?"* — impossible once that day's sessions
  were deleted: usage records live **inside** `sessions/<id>.json`, so every
  session-cleanup path (`/sessions` delete, empty-session pruning on `list`,
  wiping a whole project bucket) deletes the usage history with it.

Sessions are routinely cleaned — that is the point of cleanup — so usage
history that dies with them can never be the basis for honest daily
reporting. What is needed is a **sibling store**: usage facts recorded at the
moment they happen, persisted *next to* (never inside) the session data, so
no session lifecycle can touch them.

## Decision

1. **A day-partitioned, append-only store lives at `data/usage/`.** One JSON
   file per local day (`usage/daily/<YYYY-MM-DD>.json`), each holding the
   terminal `RequestUsageRecord`s booked that day plus the wall-clock settle
   time and the project bucket name. The root sits directly under the data
   directory — a **sibling of `projects/`** — so deleting sessions, pruning
   empty ones, or removing a project bucket cannot touch it. Day boundaries
   follow the local timezone (`chrono::Local`): "daily usage" is a human
   concept.

2. **The sink hangs off the existing settlement point.** `TokenSourceLedger`
   gains an optional `UsageStatSink` (`install_usage_sink`). `settle_request`
   — already the single place every attempt reaches a terminal state
   (completed / interrupted / failed / abandoned, reported or estimated) —
   forwards each settled record to the sink after the ledger update. No
   provider, agent-loop, or orchestration code changes: the daemon bootstrap
   installs `UsageStatsStore` as the sink once per process, stamped with the
   project bucket name.

3. **Idempotent, monotonic appends.** Within a day file, records are keyed by
   `RequestUsageKey`; a replayed key is a no-op, and a *reported* replay
   upgrades an earlier *estimated* record in place (an estimate can never
   downgrade a reported one) — the same monotonic rule the in-memory ledger
   applies. Writes are atomic (temp-file + rename) and serialised across
   processes by the companion `FileLock`. An unreadable day file is skipped
   with a warning: usage telemetry must never take the app down.

4. **`/usage` opens the report overlay.** A new builtin slash command
   (intercepted in the TUI, listed in completion/`/help`) opens a modal fed
   by `AgentRequest::QueryUsageStats` → `AgentResponse::UsageStatsReport`.
   The handler aggregates the store into three sections — per-day totals
   (with a two-week bar chart), per-`(provider, model)` totals sorted by
   descending usage, and the recent terminal-request event log (state,
   model, tokens, local time). The reply carries no session id: the data is
   session-independent by design.

5. **No monetary cost layer.** Consistent with the token-accounting
   explanation's standing decision, the report is denominated in tokens and
   counts only. Pricing tables are out of scope.

## Consequences

- Usage history survives every session-cleanup path by construction, and the
  store is append-mostly: a day file is rewritten (atomically) only when a
  later replay upgrades an estimate inside it.
- The store grows one small file per active day; `all_records` bounds a
  query to the newest 400 day files (~13 months) so a `/usage` open stays
  cheap regardless of history depth.
- `record_invocation`-style command telemetry is *not* mirrored here — only
  model-request usage. Command usage remains session-scoped.
- The web panel can reuse the same aggregate: `UsageStatsReport` is a
  serialisable contract type (ts-rs generates its TS shape), so a future
  surface only needs to send `QueryUsageStats`.
- Backfilling history from existing session files is possible (the
  aggregation helper `aggregate_usage_records` is public) but deliberately
  not done at startup: it would couple boot to a filesystem walk for data
  the user has not asked to see.
