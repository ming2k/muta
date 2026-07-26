# Pursuits and the pursue stop-gate

A **pursuit** is a durable, per-session objective. `/pursue <condition>` arms a
**stop-gate** that keeps the agent working toward that objective until the
model signals it is done — the autonomous "pursuit" is *within-round*
continuation, not an outer loop of whole rounds. This page is the mechanism
deep dive; for where it fits in the control plane see
[Harness architecture](harness.md), and for the clock-driven counterpart see
the [`/repeat` cron scheduler](#the-repeat-cron-scheduler) section below.

## Why a dedicated primitive

Without a pursuit, an agent round is stateless: the model decides when a task is
done by emitting a final message. Long, multi-step work needs more than that:

1. **Durable intent.** An objective stated up front must still be active after
   a restart. The pursuit persists on the session store (`SessionData.pursuit`,
   ADR-0032), so it survives `/resume` and process restarts.
2. **A driver that does not give up early.** A single round ends the moment the
   model stops calling tools, which often happens long before a real objective
   is achieved. The stop-gate refuses to let the round end until the condition
   is met (or a safety cap is hit).
3. **A trusted termination signal.** The driver needs a structured "the
   objective is genuinely done" signal it can trust, distinct from a routine
   end-of-round.

The pursuit does not carry one flattened status enum or a checklist. Its state
is split by ownership: the durable objective record, the runtime stop-gate
attempt, and an observability checkpoint. Earlier revisions had a user-only
status enum and a second checklist completion gate; both were removed.
Budgets later returned in opt-in form: the user may set hard pursuit-pass
(`passes=`), token, or wall-clock limits with `/pursue budget …`, and reaching
one stops the attempt with a terminal reason. The former `turns=` spelling is
accepted only for compatibility. See
[ADR-0010](../../adr/0010-slim-goal-primitive.md),
[ADR-0015](../../adr/0015-pursue-stop-gate-and-repeat-cron.md), and
[ADR-0069](../../adr/0069-pursuit-budgets-and-stats.md) for that history.
[ADR-0083](../../adr/0083-crash-consistent-pursuit-attempt-accounting.md)
defines the current crash-consistent attempt accounting.

## Three aligned state layers

The layers are related, but they are not interchangeable axes of one large
state machine:

| Layer | Subject | State | Authority |
|-------|---------|-------|-----------|
| Durable objective | The pursuit across attempts | objective, `is_complete`, optional budget, optional `terminal_reason` | session event log + snapshot |
| Runtime attempt | One armed stop-gate execution | `armed`, forced-continuation count, passes/tokens/active time | agent memory mirrored into the session runtime |
| Checkpoint projection | `/session status` observability for one attempt | one-based pursuit pass, 50-pass maximum, running/completed/interrupted/error | session checkpoint |

`Option<Pursuit>` distinguishes “no objective” from an objective. For an
incomplete objective, `terminal_reason = None` means no failed/stopped attempt
is currently recorded; a reason names why the latest attempt stopped.
Successful completion sets `is_complete = true` and clears the terminal
reason. Re-arming an incomplete pursuit also clears the old attempt reason
before work begins.

Runtime counters are crash-consistent with continuation save points. They are
restored with the armed flag so token/time/pass budgets do not reset after a
process restart.

## Pursuit interface

The pursuit lifecycle has three phases, each owned by one role. There are no
model-facing pursuit tools (ADR-0031): the entry, continuation, and exit are
mechanisms, not tool calls.

| Role | Responsibility | Mechanism |
|------|----------------|-----------|
| **User** | Set the condition (entry) | `/pursue <condition>` slash command |
| **Harness** | Drive + gate (continuation) | stop-gate re-injects the condition each turn |
| **Model** | Signal completion (exit) | `[NEENEE_PURSUIT_COMPLETE]` marker |

The active `objective` is surfaced in the system prompt each round for
visibility, but the system prompt no longer advertises any pursuit tools.

## The pursue stop-gate

`/pursue <condition>` does three things: persists the condition as the active
pursuit, **arms the stop-gate** on the agent, and drives one agent round. The
gate sits at the turn-loop exit. On each exit it consults
`pursuit_continuation`, which returns a continuation prompt when **all** of
these hold:

- a pursuit is armed;
- an active (incomplete) pursuit exists;
- the latest response did **not** signal completion;
- no configured budget has been reached;
- another pursuit pass would remain within the safety cap.

When it returns a prompt, the gate injects the condition as a hidden
user-role message, bumps its iteration counter, and forces another turn
instead of returning. The round therefore runs across many turns, re-injected
each time the model tries to stop.

```text
/pursue make all tests pass and CI green
  └─ pursuit persisted; previous terminal reason cleared; stop-gate armed

  turn 1: model edits code, then tries to end the round
    └─ gate: armed, pursuit incomplete, no completion signal → re-inject condition → turn 2

  turn N: model verifies, emits [NEENEE_PURSUIT_COMPLETE]
    └─ gate sees the completion signal → lets the round end
    └─ orchestration finalizes: is_complete = true; terminal_reason = none
```

### Completion is a signal, not a judgement

There is no separate LLM "is the condition met?" judge on each stop. The
working model itself signals completion — by emitting the
`[NEENEE_PURSUIT_COMPLETE]` marker — and the gate trusts that signal (the gate
*gates*, the model *signals*). This matches Claude Code's stop-hook `/pursuit`,
avoids a model call per stop, and keeps the decision deterministic.

The marker is the sole completion path. It is always stripped from visible
output — it is a control signal, not prose. (`/pursue done` remains the
user-driven completion slash command for interactive rounds.)

### Safety cap

One explicitly armed pursuit attempt is bounded to 50 **pursuit passes**. A
pass ends whenever the model would naturally end the round and the gate must
decide whether to force continuation; tool-calling turns inside a pass are not
separate pursuit iterations. Before a 51st pass could begin, the gate disarms
and records a terminal reason naming the safety cap. This is not the removed
default cap on ordinary agent rounds: normal ReAct work remains uncapped
unless the user configures a hard stop; the 50-pass bound applies only to the
opt-in autonomous stop-gate.

The user can interrupt at any time with `Esc` or `/pursue stop`. Budget,
safety-cap, interruption, supersession, and provider/tool errors all leave the
objective incomplete and record a reason for the stopped attempt. `/pursue`
can re-arm that objective; `/pursue done` completes it explicitly; `/pursue
clear` removes it.

## The `/repeat` cron scheduler

Orthogonal to pursuits, `/repeat <cron> <prompt>` schedules a prompt on a
**clock**. It is a fully separate subsystem — the two driving dimensions are
deliberately distinct:

| | `/pursue` | `/repeat` |
|---|---|---|
| Driver | a condition (stop-gate) | a clock (cron) |
| Work unit | turns within one round | a fresh round per tick |
| Stops when | the condition is met / cap / interrupt | cancelled or auto-expired |
| Persistence | pursuit on the session store | jobs in `repeat.db` |

`/repeat` parses a five-field cron expression (`minute hour day month weekday`,
e.g. `*/5 * * * *` for every five minutes, `0 9 * * 1-5` for 09:00 on
weekdays), stores the job durably, runs the first fire immediately, and a
background scheduler ticks every 30 s to fire due jobs as fresh chat rounds.
Jobs auto-expire after 30 days. See [Slash commands](../../reference/commands.md)
for the command surface.

## Persistence

The pursuit lives as a `pursuit: Option<Pursuit>` field on `SessionData`
(ADR-0032), the event-sourced per-session store. Resuming the same session
restores the same pursuit — there is no separate "pursuit resume" step and no
separate database; pursuit, todos, title, and checkpoints all share one
session file (`<id>.json` snapshot + `<id>.jsonl` event log).

Older installations may still have a `pursuits.db` file (the pre-ADR-0032
store) or `harness_goal*` config keys (pre-ADR-0010) on disk. Both migration
paths have been removed (ADR-0082): the file and keys are never read, and a
pursuit is re-set with `/pursue` after upgrading across the window.

A checkpoint is updated at continuation save points and on termination. Its
status vocabulary is typed (`running`, `completed`, `interrupted`, `error`);
unknown future values load through a compatibility fallback. Its iteration and
maximum are both measured in one-based pursuit passes. It is an observability
projection, not the source of truth for completion.

The objective record and runtime attempt are the recovery authorities:
continuation boundaries persist the injected prompt, armed flag, continuation
count, and budget counters together. On terminal paths the runtime is persisted
as disarmed, so resuming a completed or stopped attempt cannot silently restart
it.

## See also

- [Harness architecture](harness.md) — the control plane, the stop-gate's
  place in the round loop, and how completion interleaves with retry and
  cancellation
- [Built-in tools](../../reference/tools/index.md) — the pursuit interface has
  no model-facing tools; see [pursuits](../../reference/tools/pursuits.md)
- [Slash commands](../../reference/commands.md) — `/pursue` and `/repeat`
- [ADR-0015](../../adr/0015-pursue-stop-gate-and-repeat-cron.md) — the
  decision to replace `/goal` + `/loop` with the stop-gate + cron scheduler
- [ADR-0010](../../adr/0010-slim-goal-primitive.md) — slimming the pursuit
  primitive
- [ADR-0031](../../adr/0031-pursuit-tools-removed.md) — removing the
  model-facing pursuit tools
- [ADR-0032](../../adr/0032-fold-pursuit-into-session-store.md) — folding
  pursuit persistence into `SessionStore`
- [ADR-0082](../../adr/0082-contain-pursuit-behind-the-stop-gate.md) —
  containing pursuit behind the stop-gate; removing the legacy migrations
- [ADR-0083](../../adr/0083-crash-consistent-pursuit-attempt-accounting.md) —
  persisted attempt counters and aligned pursuit-pass semantics
