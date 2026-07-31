# 0090. Scheduled-prompt unification: cron + one-shot timers

- **Status:** Accepted
- **Date:** 2026-07-31

## Context

The harness already had a clock-driven scheduled-prompt subsystem: the
`/repeat` command (`crates/neenee-transport/src/handlers_slash.rs`) scheduled a
prompt on a five-field cron expression, stored jobs as session-scoped state
(`SessionEvent::RepeatJobsSet`, session-schema v8), and a background scheduler
(`start_repeat_scheduler` in `crates/neenee-agent/src/orchestration.rs`) fired
each due job as a normal `AgentRequest::Chat` round every 30 s.

What was missing was the **one-shot** case: "run this prompt once, after a
countdown, or at a specific system time" — e.g. a quota reminder that kicks off
a new round 10 minutes from now, or a task that should start at 14:00. The
`/repeat` cron model is the wrong shape for this: a one-shot is a single future
fire instant, not a recurring schedule, and forcing it through cron (e.g.
encoding a future instant as a cron that then matches exactly once) is awkward
and leaks recurring semantics (auto-expiry, next-fire advancement) into a thing
that should fire exactly once and disappear.

The envoy survey of the scheduler subsystem confirmed the existing
infrastructure is the right thing to extend: `RepeatJob` is the value type,
`SessionStore::repeat_jobs`/`set_repeat_jobs` is the persistence, the 30 s
`tokio::time::interval` tick is the driver, and `AgentRequest::Chat` is the
dispatch egress. All of this is "round-level connection" machinery shared with
the loop/outbox — a scheduled fire produces exactly the same
`AgentRequest::Chat → execute_round → RoundCompleted` flow as a hand-typed
prompt or a queued outbox dispatch.

## Decision

Generalize the scheduled-prompt subsystem into a unified model that handles
both recurring (cron) and one-shot (countdown / absolute-time) jobs, reusing
the existing persistence, scheduler, and dispatch path.

1. **Unified value type.** Replace `RepeatJob { cron: String, … }` with
   `ScheduledJob { trigger: Schedule, … }`, where
   `Schedule = Cron { cron } | Once { fire_at }` (in
   `crates/neenee-core/src/repeat.rs`). `Schedule::next_fire(now)` computes the
   next fire instant for either variant. `RepeatJob` is kept as a thin newtype
   alias for source-level back-compat.

2. **Time-expression parser.** Add `parse_schedule_arg(raw, now) →
   ScheduleAt` in the same module, accepting a five-field cron, a relative
   countdown (`10m`, `2h30m`, `in 2 hours 30 minutes`), or an absolute time
   (`14:00`, `tomorrow 09:00`, `2026-03-15 14:00`). Cron is detected
   structurally (exactly five fields) so it never collides with the time forms.

3. **Single command.** Add `/schedule <when> <prompt>` as the unified entry
   point. `/repeat` is retained as a cron-only alias that funnels into the same
   add/list/cancel paths.

4. **Generalized scheduler.** Rename `run_repeat_tick`/`start_repeat_scheduler`
   to `run_schedule_tick`/`start_schedule_scheduler`. The tick now advances a
   cron job's schedule (as before) **or** drops a once-job after it fires.
   Once-jobs are exempt from the 30-day recurring-job age cutoff (a one-shot
   future fire is its own expiry).

5. **Back-compat persistence.** Rename the session field `repeat_jobs` →
   `scheduled_jobs` with `#[serde(alias = "repeat_jobs")]`, and the event tag
   `repeat_jobs_set` → `scheduled_jobs_set` with `#[serde(alias =
   "repeat_jobs_set")]`. `ScheduledJob` has a manual `Deserialize` that accepts
   both the new tagged shape (`{"kind":"cron"|"once", …}`) and the legacy flat
   shape (`{"cron":"*/5 * * * *", …}`). Bump session-schema v8 → v9; no payload
   transformation is needed, only the version bump records the rename.

6. **Round-level reuse.** A scheduled fire reuses the exact same dispatch egress
   as `/repeat` did and as the outbox does: `AgentRequest::Chat` flows through
   `execute_round` and emits `RoundCompleted`. No new round machinery is
   introduced — this is deliberately a "round-level connection" reuse, not a
   new loop kind.

## Alternatives considered

- **Separate once-job type and scheduler.** Add a parallel `OnceJob` value, a
  second persistence slot, and a second tick. Rejected: it duplicates the
  scheduler, the persistence, and the dispatch egress for no gain. The envoy
  survey explicitly recommended modeling new scheduling on `/repeat` and reusing
  its `tokio::time::interval` task and `AgentRequest` egress.

- **Force one-shots through cron.** Encode a future instant as a cron that
  matches exactly once. Rejected: leaks recurring semantics (30-day auto-expiry
  would delete a far-future one-shot; next-fire advancement is meaningless for a
  job that should fire once), and the UX is hostile ("compute the minute/hour/
  day/month/weekday for 2026-03-15 14:00").

- **`ScheduledJob` as an enum, not a struct with a `trigger` field.** Rejected:
  a struct with a `trigger` field keeps the shared `id`/`prompt`/`created_at`/
  `next_fire`/`last_fire` bookkeeping in one place and lets `#[serde(flatten)]`
  produce a clean tagged wire shape.

## Consequences

- **Positive.** One unified, durable, resumable scheduled-prompt system covers
  recurring cron *and* one-shot timers. The countdown/absolute-time scenario
  (quota reminders, delayed task kickoff) is now first-class. Round-level
  dispatch is unchanged, so all existing round/lifecycle/outbox guarantees
  (generation-guarded at-most-one round, natural-completion outbox pause) apply
  identically to scheduled fires.

- **Positive.** `/repeat` keeps working as a cron-only alias, so existing user
  muscle memory and any documented cron examples are unaffected.

- **Neutral.** Session schema moves to v9. Legacy v8 snapshots and event logs
  load unchanged (serde aliases + the legacy-flat-shape deserializer); the
  version bump is the only record of the rename. `RepeatJob` remains as a
  source-level newtype so in-tree references keep compiling during the
  transition.

- **Migration.** No user action required. On first open after upgrade, a v8
  session's `repeat_jobs` field deserializes into `scheduled_jobs` as cron
  `Schedule::Cron` jobs and the schema bumps to v9 on the next persist. New
  once-jobs are only creatable via `/schedule`.

## References

- ADR-0009 (uncapped agentic loop) — the project stance that a round ends when
  the model stops calling tools; scheduled prompts are an *input* trigger, not
  a loop bound.
- ADR-0082 (remove pursuit stop-gate) — removed the last budget/loop primitive;
  `/repeat` (now `/schedule`) was kept as the only scheduled-prompt mechanism.
- ADR-0024 (pragmatic SQLite migrations) — the `repeat_jobs` → session-state
  migration pattern this change extends.
- `crates/neenee-core/src/repeat.rs`, `crates/neenee-core/src/cron.rs`,
  `crates/neenee-agent/src/orchestration.rs`,
  `crates/neenee-persistence/src/session/mod.rs`,
  `crates/neenee-transport/src/handlers_slash.rs`.
