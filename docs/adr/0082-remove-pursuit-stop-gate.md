# 0082. Remove the pursuit stop-gate and primitive

- **Status:** Accepted
- **Date:** 2026-07-26

## Context

The **pursuit** primitive was a durable, per-session objective that `/pursue
<condition>` armed as a **stop-gate**: when the model would end a round, the
gate re-injected the condition as a hidden user message and forced another
turn, until the model emitted a `[NEENEE_PURSUIT_COMPLETE]` marker, a 50-pass
safety cap was hit, a budget tripped, or the user interrupted. It also carried
an optional budget (`passes` / `tokens` / `time`), a terminal reason, and a
crash-consistent attempt runtime, all persisted on the session store.

Across the design review the primitive was peeled apart into three layers and
each was weighed on its own:

1. **The durable objective + system-prompt anchor.** This was argued as a
   hedge against context compaction dropping the objective. But the objective
   already lives in the transcript (`messages` array), which `/resume`
   restores verbatim — the field double-counted that. The only real value,
   surviving compaction, is better solved by making compaction preserve the
   opening objective than by carrying a parallel persisted field.

2. **The completion marker.** The marker is not a protocol patch: an ordinary
   round ends when the model stops calling tools, and a capable model stops
   precisely when it is done. The marker exists only as the *exit valve* for
   the gate — once the gate is gone, the marker has nothing to unlock and is
   redundant.

3. **The stop-gate (forced continuation) + safety cap + budget.** This is the
   only layer with real purpose, and its purpose is exactly one thing:
   **autonomy** — letting the agent keep working while no human is at the
   keyboard to send the next prompt. That is a capability compensation for a
   model that stops early, not a protocol obligation. It earns its keep only
   when (a) the model is weak/edge-side enough to quit prematurely, or (b) the
   user wants to walk away and come back to finished work.

The conclusion: layers 1 and 2 are not load-bearing, and layer 3 is an
opt-in autonomy feature — not a structural necessity. The default product
shape ("a capable model + a human at the keyboard") is best served by the
simplest round model: **a round ends when the model stops calling tools, and
that is treated as completion.** Forcing continuation is the model's
responsibility to need, not the client's to perform.

Reference implementations were checked: neither codex nor Claude Code carry a
durable goal primitive on the round loop, and Claude Code's reverse-engineered
`/goal` stop-hook is an opt-in autonomy lever, not the default round shape.

## Decision

Remove the pursuit primitive entirely:

- Delete `/pursue` and all its subcommands (`status`, `stop`, `done`, `clear`,
  `edit`, `budget`, empty re-arm). The `BuiltinCmd::Pursue` variant and the
  `/pursue` slash-completion entries are gone.
- Delete the stop-gate machinery: `PursuitState`, `pursuit_continuation`,
  `stop_gate`'s pursuit arm (the `Stop`-hook arm remains), `book_pursuit_pass`,
  the convergence reminder, the 50-pass `MAX_PURSUIT_ITERATIONS` cap, and the
  `[NEENEE_PURSUIT_COMPLETE]` marker (detection, stripping, and the
  completion-persist block in `execute_round`).
- Delete the durable types: `Pursuit`, `PursuitBudget`, `PursuitCheckpoint`,
  `PursuitCheckpointStatus`, `PursuitRuntime`, `UNCAPPED_ITERATIONS`.
- Delete the persisted fields and events: `SessionData.{loop_checkpoint,
  pursuit, pursuit_runtime}` and the `PursuitSet`, `PursuitRuntimeSet`, and
  `CheckpointSet` session events. The `SessionStore::{pursuit, set_pursuit,
  mark_pursuit_complete, update_pursuit_objective, pursuit_runtime,
  set_pursuit_runtime, checkpoint, set_checkpoint}` methods are removed.
- Delete the system-prompt `PursuitObjective` section and the
  `SystemPromptContext.pursuit` field.
- `execute_round` now returns `Result<(), HarnessError>` instead of
  `Result<bool, HarnessError>`; the bool was the pursuit-completion flag and
  has no meaning without a pursuit.
- The `LoopStatus::Pursue` variant is removed (the activity bar no longer
  distinguishes "pursue" from "running"); `RoundEvent::PursuitUpdated` /
  `PursuitCleared` and `AgentEvent::PursuitUpdated` are removed; the
  `HarnessSnapshot.pursuit` field is removed.
- The `InjectionKind::PursuitContinuation` / `PursuitObjectiveUpdated`
  variants are removed.
- The `/export` metadata header drops its **Pursuit** line.

`/repeat` (the cron scheduler) is **kept**: it is mechanism-orthogonal (a
clock, not a stop-gate) and genuinely useful for unattended scheduled prompts.
The misplaced SessionStart-hook block that used to live inside `/pursue
status` is relocated to `bootstrap::assemble`, where it fires once at startup
— its prior location was a known bug (it fired on every `/pursue status`).

A round is now: a capable model runs the ReAct loop until it stops calling
tools, and that natural stop is completion. `Stop` hooks (ADR-0025) still get
a vote on whether to force another turn.

## Alternatives considered

- **Keep the stop-gate but make it opt-in per-model capability (a
  `pursuit_capable` / `self_directed` capability bit, default off on
  edge-side/fallback models).** This was the leading alternative: it keeps
  autonomy for the weak-model/edge case while letting strong models run
  unaided. Rejected for this ADR because it preserves the full surface area
  (gate + marker + budget + checkpoint + persistence + TUI badge) for a
  minority opt-in use case, and the review's conclusion was that autonomy is
  a product question, not a structural one — it can return as a focused
  feature if a real unattended-mode need emerges, rather than living as the
  default round machinery.
- **Keep the durable objective + marker as a pure system-prompt anchor, drop
  only the gate.** Rejected: the anchor's only real value (surviving
  compaction) is better owned by compaction itself, and the marker without the
  gate is a key with no lock. Keeping half the primitive would preserve the
  conceptual confusion for no benefit.
- **Add a `verify` hook (run a shell command to judge completion) as the
  replacement completion path, keeping the gate to drive another turn on
  verify failure.** Rejected as conflating two things: verify is a health
  report orthogonal to forcing work, and tying it to the gate reintroduces the
  exact mechanism this ADR removes. If verification is wanted later it can be
  a standalone stop-time check that *reports* without *forcing*.

## Consequences

Positive:

- One round shape, with no special autonomous mode: a round ends when the
  model stops calling tools, and that is completion. The harness is smaller
  and the round model is trivially predictable.
- ~260 lines of gate logic (`pursuit_state.rs`), the `start_pursuit` driver,
  the pursuit prompts, six SessionStore methods, three session events, three
  persisted fields, two wire events, two injection kinds, and a system-prompt
  section are gone. The `execute_round` return type is honest.
- The misplaced SessionStart-hook block is fixed.

Negative:

- **Autonomy is gone.** A user can no longer set a condition and walk away
  expecting the agent to keep working unprompted. Mitigation: a capable model
  completes long tasks within one round by its own tool-calling; `/repeat`
  covers scheduled/unattended prompt delivery; and a focused unattended-mode
  feature can be re-introduced if the need is real.
- **Breaking change for `/pursue` users.** Any muscle memory, scripts, or
  exported-session markers referencing `/pursue` or `[NEENEE_PURSUIT_COMPLETE]`
  must migrate. `/pursue` is no longer a command; the marker is ignored.
- **Legacy session files** carrying `pursuit` / `pursuit_runtime` /
  `loop_checkpoint` fields and `PursuitSet` / `PursuitRuntimeSet` /
  `CheckpointSet` events still load: serde ignores unknown fields and unknown
  event variants under the forward-compat fallback, so resume is not broken —
  the pursuit state is simply discarded.

Neutral:

- `LoopStatus` collapses to `Idle` / `Running`; the activity bar no longer
  shows a "pursue" state.
- The `/repeat` cron scheduler is unaffected and remains the only scheduled-
  prompt mechanism.

## References

- [Harness architecture](../explanation/agent-design/harness.md) — the
  remaining round machinery and the `Stop`-hook gate that survives.
- [Chat API primitives](../explanation/chat-api-primitives.md) — why an
  ordinary round ends when the model stops calling tools.
- ADR-0025 (lifecycle hooks) — the `Stop` hook, now the only turn-end
  continuation lever.
- Supersedes the removed ADR-0010 (slim goal primitive), ADR-0015 (pursue
  stop-gate + repeat cron), ADR-0031 (pursuit tools removed), ADR-0032 (fold
  pursuit into session store), ADR-0069 (pursuit budgets and stats),
  ADR-0083 (crash-consistent pursuit attempt accounting).
