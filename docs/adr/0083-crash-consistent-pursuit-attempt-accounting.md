# 0083. Crash-consistent pursuit attempt accounting

- **Status:** Accepted
- **Date:** 2026-07-26

## Context

ADR-0069 added pursuit budgets and runtime statistics but kept
`PursuitStats` in memory, rebuilding only the continuation count on resume.
That is insufficient for a token or wall-clock budget: neither value can be
derived from the continuation count after a crash. A restored armed pursuit
could therefore receive a fresh budget even though the preceding process had
already consumed part of it.

The three pursuit representations had also drifted in granularity:

- the stop-gate counted forced continuations;
- the checkpoint always reported iteration `1` with an uncapped sentinel;
- budget messages called the unit a turn even though one gate decision may
  follow several tool-calling ReAct turns;
- terminal reasons were not written on every non-completion path.

ADR-0048 already makes the session the source of truth for the armed flag and
continuation count. Extending that existing runtime record is smaller and more
consistent than adding a second statistics store or a project-wide state
machine.

## Decision

Keep pursuit split into three subjects with separate authority:

1. `Pursuit` is the durable objective: objective text, completion bit, optional
   budget, and the latest non-completion terminal reason.
2. `PursuitRuntime` is one execution attempt: armed flag, forced-continuation
   count, pursuit-pass count, tokens, and active wall-clock time.
3. `PursuitCheckpoint` is an observability projection: one-based pursuit pass,
   the 50-pass safety maximum, and a typed attempt status.

Persist runtime counters at the same continuation save points as the
transcript and restore them exactly with an armed attempt. A fresh objective
or explicit re-arm resets the counters and clears a stale terminal reason;
resuming a crash-restored armed attempt preserves them. New runtime fields use
serde defaults so older snapshots load with zero counters.

Define a **pursuit pass** as the work between two natural round-stop decisions.
Tool-calling ReAct turns inside that work are not separate pursuit iterations.
The internal `iterations` value continues to count continuations already
forced; the checkpoint projects `min(iterations + 1, 50)` so its iteration and
maximum use the same one-based unit. `/pursue budget passes=N`, `max_passes`,
and persisted runtime `passes` are canonical. The former command key `turns`,
budget field `max_turns`, and runtime field `turns` remain read-compatible.

Make checkpoint status a typed, forward-compatible vocabulary:
`running`, `completed`, `interrupted`, `error`, and an `unknown` fallback for
legacy or future values. The checkpoint reports an attempt; it does not decide
objective completion.

Every non-completion terminal path disarms the attempt and records a reason.
Completion sets the objective complete and clears the reason. A later generic
interrupt must not overwrite an earlier, more specific budget or safety-cap
reason.

This supersedes ADR-0069's decision to keep pursuit statistics unpersisted.
Its remaining decisions stay in force: budgets are opt-in, completion remains
marker-based, there is no LLM judge, and the convergence reminder is advisory.

## Alternatives considered

- **Rebuild all counters from the continuation count.** Rejected because token
  and elapsed-time consumption cannot be reconstructed from an iteration
  number.
- **Count every provider request as a pursuit iteration.** Rejected because it
  couples pursuit policy to tool-call depth and does not match the stop-gate
  boundary where continuation is decided.
- **Use the checkpoint as execution authority.** Rejected because a reporting
  projection should not compete with the objective and runtime records.
- **Create one global session state machine.** Rejected for the reasons in
  ADR-0078: parked requests, presentation state, and pursuit attempts have
  different subjects and owners.

## Consequences

- Token, time, and pass budgets remain monotonic across crash recovery.
- Runtime, checkpoint, status output, and safety-cap messages use one pursuit
  pass unit.
- Snapshot compatibility is additive: legacy runtime counters default to zero,
  and unknown checkpoint statuses remain loadable.
- Continuation save points perform small additional session writes, skipped
  when the projected runtime and checkpoint are unchanged.
- New command output and persistence use `passes`; older `turns` and
  `max_turns` inputs remain load-compatible without preserving the ambiguous
  spelling on the next write.

## References

- [ADR-0047](0047-round-contains-turn-vocabulary.md) — current round/turn
  vocabulary
- [ADR-0048](0048-session-as-single-source-of-truth.md) — persisted
  session-scoped runtime state
- [ADR-0069](0069-pursuit-budgets-and-stats.md) — superseded in-memory
  pursuit-statistics decision
- [ADR-0078](0078-round-lifecycle-type.md) — scoped lifecycle protocols rather
  than one global state machine
- [Pursuits and the pursue stop-gate](../explanation/agent-design/pursuits.md)
- [State and status model](../reference/state-model.md)
