# 0110. Commands do not trigger the activity bar

- **Status:** Accepted
- **Date:** 2026-08-18
- **Revises:** [ADR-0092](0092-guaranteed-activity-resolution.md) §2 (the
  TUI no longer paints *any* optimistic activity for control-plane
  dispatches, not even transiently) and §3's premise (the optimistic state
  that needed careful retirement is gone for commands)

## Context

The activity bar (ADR-0008's single liveness anchor) is the projection of
the **round state machine**: it is up while `RoundLifecycle` reports a live
round, its label comes from round events (`preparing context`, `waiting for
model`, tool activity, …), and its trailing hint offers `Esc Esc interrupt`
— an affordance that only means something for a round.

A slash command is **not part of the round state machine**. It is a
synchronous control-plane operation (`handlers_slash::dispatch` runs inline
in the driver loop; `round_owned_request` classifies `SlashCommand` as
non-round). But two code paths made commands light the bar anyway:

1. **The TUI armed optimistic activity at dispatch.**
   `SendSlash` (`crates/neenee-tui/src/event_loop/actions/commands.rs`) set
   `is_responding = true` and `activity_status = "queued"` whenever the
   viewed session was idle — the same arming a chat send performs. The bar
   then showed the breathing dot and `Esc Esc interrupt` for a command that
   (a) is not interruptible, (b) did not start a round, and (c) typically
   completes in milliseconds. ADR-0092 §2 had already restricted this to
   the idle case and made the driver reconcile it away, but the transient
   itself remained: every command still *flashed* a fake round state.
2. **Some command handlers borrowed `RoundEvent::Activity`.**
   `/compact` emitted `Activity("compacting context")` while running inline
   in the driver (and `/review`, since retired as a command, emitted
   `Activity("running session review…")` the same way).
   The TUI's listener arms `is_responding` on *every* `Activity` event, so
   these lit the bar for the duration of the command — and, dispatched
   mid-round, would have **overwritten the running round's live label**
   with the command's progress (exactly the defect class ADR-0092 §2
   described for the optimistic paint).

The user-visible defect this ADR answers: *command 不应该触发 activity
bar，command 不属于状态机的一部分。* A command's feedback belongs to its own
component — the ADR-0108 command row, whose **Pending** phase (`⌘ /cmd`,
muted, markerless) already says "dispatched, awaiting reply" in the
transcript itself — not to the round liveness surface.

## Decision

### 1. The TUI never arms activity state for a command dispatch

`SendSlash` performs **no** activity-bar side effects: it does not set
`is_responding`, does not paint `activity_status`, and does not consult
`running_sessions`. A running round keeps owning the bar through its own
events (nothing is painted over it); an idle harness stays idle (no
transient flash). The pending command row (ADR-0108) is the command's
in-flight feedback.

The `/serve` sub-branch, which resolves entirely in the TUI, drops its
symmetric retirement of the optimistic state — there is nothing left to
retire.

### 2. Command handlers stop emitting `RoundEvent::Activity`

`/compact` no longer emits its `Activity` event. A command's outcome is
its typed result (`RoundEvent::CommandResult`, which settles the command
row) — its progress is not round activity. `RoundEvent::Activity` remains
reserved for round-owned emitters: the round task in
`crates/neenee-agent/src/orchestration.rs` (including the automatic
in-round `compacting context` step, which *is* part of the state machine)
and the shell-command round.

### 3. The driver reconcile stays as a safety net

`SessionDriver`'s post-dispatch reconcile (ADR-0091/0092 §1) is unchanged:
every non-round request still lands the harness back in its authoritative
`HarnessState`. The TUI no longer needs it for commands, but the invariant
is structural and covers any frontend that still paints optimistic state
(the web panel, future clients) — for the TUI it is now an idempotent
no-op.

## Alternatives considered

- **Keep the transient, rely on the driver reconcile** (the ADR-0092
  status quo). Rejected: the reconcile guaranteed the bar *returns* to
  idle, but the flash itself was still fabricated liveness — a lie with a
  guaranteed expiry is still a lie.
- **Give commands a real (short-lived) round.** Rejected: commands are
  control-plane by design; routing them through `RoundLifecycle` would
  bump round counters, reset turn state, and make every command
  "interruptible" in the protocol while it is not interruptible in fact.
- **Route command progress through a new `CommandActivity` event.**
  Rejected as over-engineering: no current command is long enough to need
  a progress surface; the pending row plus the typed result already cover
  the lifecycle (ADR-0108 §2).

## Consequences

**Positive.**

- The activity bar is a truthful projection of the round state machine and
  nothing else: it is up exactly while a round is live.
- No fake `Esc Esc interrupt` affordance over a synchronous command.
- A command dispatched mid-round can no longer overwrite the running
  round's live activity label.
- Fewer events per command dispatch (no optimistic paint to retire, no
  borrowed `Activity` events).

**Negative.**

- A genuinely slow command (a large `/compact`, a deep `/review`) shows no
  liveness beyond its pending row. If one ever grows long enough to need
  it, the answer is a command-scoped progress surface, not a return to the
  round bar.

**Neutral.**

- ADR-0092's driver reconcile remains in force and unchanged; §2's TUI
  rule is tightened from "only when idle" to "never".

## Verification

- Dispatching `/autopilot on` with an idle harness: the command row goes
  pending → completed; the activity bar never appears.
- Dispatching `/compact` while a round streams in another session of the
  same view: the round's label is untouched.
- `round_owned_request` / `needs_activity_reconcile` unit tests unchanged
  and passing (the driver reconcile is not behaviorally altered).
## References

- [ADR-0008](0008-single-breathing-anchor.md) — the activity bar as the
  single liveness anchor (for rounds).
- [ADR-0092](0092-guaranteed-activity-resolution.md) — the guaranteed
  resolution this builds on; §2 revised here.
- [ADR-0108](0108-one-command-component-input-output-lifecycle.md) — the
  command row whose Pending phase is the command's in-flight feedback.
- [ADR-0078](0078-round-lifecycle-type.md) — `RoundLifecycle` /
  `LoopStatus`, the state machine the bar projects.
