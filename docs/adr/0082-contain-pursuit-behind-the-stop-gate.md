# ADR-0082: Contain pursuit behind the stop-gate

- Status: Accepted
- Date: 2026-07-24

## Context

Pursuit's footprint looks invasive — a grep touches well over a hundred
files — but most of that is the layered projection of one small mechanism: a
`Pursuit` value persisted on `SessionData` (ADR-0032), surfaced in the system
prompt, and enforced by a stop-gate at the round-loop exit (ADR-0015). The
mechanism itself is a few hundred lines.

The problem was not the projection but the hygiene around it:

- `neenee_core::pursuits` had become a junk drawer. Alongside the pursuit
  domain values it held `TokenUsage` (generic per-turn telemetry shared by
  core internals, the LLM client, the CLI, and the agent), `RoundOutcome`
  (the agent's per-turn result, used only inside `neenee-agent`), and
  `RoundTimer` (dead: its stated consumer, plan-progress timestamps, was
  removed with ADR-0033).
- `ThreadPursuit` survived as dead code — the persisted view of the
  pre-ADR-0032 `thread_pursuits` SQLite table whose store ADR-0032 deleted.
- Docs had drifted both ways: the glossary and the pursuits explainer still
  claimed "no budget" (ADR-0069 added `PursuitBudget`) and persistence "in
  SQLite keyed by session id" (ADR-0032 moved it onto the session store).
- Two one-shot legacy migrations outlived their window: the pre-ADR-0032
  `pursuits.db` reader and the pre-ADR-0010 `harness_goal*` config reader.
  ADR-0032 landed about a month and ten releases before this decision; anyone
  upgrading across the window has either migrated already or skipped too many
  versions for a best-effort fold-in to be worth keeping forever.

Nothing here changes runtime semantics. The risk to guard against is the
*next* pursuit change quietly growing new coupling, because today there is no
written rule about where pursuit may touch the loop.

## Decision

1. **Containment invariant.** Pursuit may interact with the round loop only
   through the `stop_gate` composition point in `neenee-agent` — the gate
   chain shared with `Stop` hooks. Round lifecycle, prompt caching, and
   compaction/pruning must not depend on pursuit types. Any new pursuit
   touchpoint outside the gate chain — new budget dimensions enforced
   mid-loop, mid-loop interventions, a new coupling between the loop and
   pursuit state — requires its own ADR. The test: pursuit must remain
   wholesale-deletable by removing its gate-chain entry without changing loop
   semantics.

2. **Relocate the generic types out of `pursuits`.** `TokenUsage` moves to
   `neenee_core::usage` (still re-exported as `neenee_core::TokenUsage`).
   `RoundOutcome` moves into `neenee-agent` next to `Agent::run` — per
   ADR-0057, agent-only logic lives in the agent crate. The dead `RoundTimer`
   and `ThreadPursuit` are deleted outright. `neenee_core::pursuits` keeps
   only `Pursuit` and `PursuitBudget`.

3. **Delete the expired migration paths.** The pre-ADR-0032 `pursuits.db`
   reader (`neenee-persistence::legacy_pursuit`) and the pre-ADR-0010
   `harness_goal*` config reader are removed, along with the
   `paths::pursuits_db()` accessor. Users upgrading across the window re-set
   their pursuit with `/pursue`.

4. **Correct the docs.** The glossary and the pursuits explainer now describe
   session-store persistence (ADR-0032) and opt-in budgets (ADR-0069); the
   `pursuits.db` row leaves the paths reference.

## Alternatives considered

- **Keep the legacy migrations "just in case".** Rejected: permanent
  migration code is a liability — it pins dead config keys and a dead
  database schema into the live tree, and every reader is a place a future
  refactor must not break. A bounded window (one month, ten releases) plus a
  trivial manual recovery (`/pursue` again) is the right trade.
- **Move pursuit entirely into `neenee-agent`.** Rejected: `Pursuit` is part
  of the durable session schema shared by persistence, transport, and
  frontends, so it is a contract and belongs in core (ADR-0057). Only the
  agent-only pieces moved.
- **Leave `TokenUsage` in `pursuits` since it compiles.** Rejected: the
  module's name promises pursuit domain values; generic telemetry there
  invites the next unrelated type to join the drawer.

## Consequences

- `neenee_core::pursuits` shrinks to its name: `Pursuit` + `PursuitBudget`.
  The crate-root re-exports keep `neenee_core::TokenUsage` source-compatible;
  no downstream import changes were required.
- Pursuit now has an explicit, testable containment rule. Code review can
  reject loop-coupling changes by pointing at this ADR instead of taste.
- Users who have not launched neenee since before ADR-0032 lose the automatic
  pursuit migration; their objective does not carry over and must be re-set
  with `/pursue`. The old `pursuits.db` file and `harness_goal*` keys are
  simply never read — they are not deleted from disk.
- Persisted schema variants are permanent regardless: `SessionEvent::PursuitSet`
  and the `InjectionKind::Pursuit*` variants are part of the event log and
  wire formats and must keep deserializing old values. This ADR deletes live
  *read paths* for obsolete stores, never the ability to replay history.
- The containment invariant constrains future pursuit features: budget
  enforcement stays at the gate, and anything richer (e.g. mid-loop
  intervention) needs its own ADR first.

## References

- [ADR-0010](0010-slim-goal-primitive.md) — the slim pursuit primitive
- [ADR-0015](0015-pursue-stop-gate-and-repeat-cron.md) — the stop-gate
- [ADR-0031](0031-pursuit-tools-removed.md) — no model-facing pursuit tools
- [ADR-0032](0032-fold-pursuit-into-session-store.md) — pursuit on
  `SessionStore`
- [ADR-0057](0057-contract-only-core-boundary.md) — contract-only core
  boundary
- [ADR-0069](0069-pursuit-budgets-and-stats.md) — pursuit budgets and stats
- [Pursuits and the pursue stop-gate](../explanation/agent-design/pursuits.md)
