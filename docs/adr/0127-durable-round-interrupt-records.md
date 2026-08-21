# ADR-0127: Durable round-interrupt records with reason and timestamp

- **Status:** Accepted
- **Date:** 2026-08-21

## Context

A round can stop before its natural terminal path (`RoundCompleted`) for
three families of reasons: the user stops it explicitly (double-Esc /
`AgentRequest::Interrupt`), a newer round replaces it (a new message, a
`!command`, a session switch — the generation-superseded path of
ADR-0078), or the host process dies with the round in flight (daemon
stop, SIGKILL, panic).

None of these left a durable trace:

- The user-interrupt path rendered an ephemeral `"... [Interrupted]"`
  `RoundEvent::Text` that is never persisted and disappears on resume.
- The superseded path was **completely silent by design**: the stale
  round's generation-guarded cleanup is suppressed (ADR-0078), so a round
  killed by a new message left no output at all — the transcript read as
  if the round never existed, with the user's (now dropped) prompt
  followed directly by the newer message.
- Process death left at most a request-ledger residue
  (`InFlight` → `Abandoned` on the next load) that only the token report
  sees; the transcript showed a dangling round with no explanation.

The consequence was concrete: a resumed session could not answer "this
round stopped — why, and when? — should I continue?" The user had to
reconstruct the story from context.

Meanwhile, the deliberate decision of
[Interrupt semantics](../explanation/interrupt-semantics.md) — *no
"interrupted" marker in the model-visible context* — remains correct: a
context marker costs tokens on every subsequent round, the omission is
already informative to the model, and markers steer the model. Any record
must therefore be **projection state**, like the command ledger
(ADR-0091), never conversation state.

## Decision

Record every round stop durably, with a closed reason classifier and a
timestamp, as **session-store projection state** that re-projects into the
transcript on resume.

### 1. Contract: `RoundInterrupt` + `RoundEvent::RoundInterrupted`

`neenee_contracts::RoundInterrupt { reason, at_ms, round: Option<u64> }`
with `RoundInterruptReason ∈ { user, superseded, terminated }`. The live
twin `RoundEvent::RoundInterrupted(RoundInterrupt)` is emitted exactly
once per stopped round, after the round's own cleanup, on **every**
interrupted path — including the generation-suppressed supersede arm and
the Phase-1 unsend (which returns `Ok(())` after `UnsentInput`).

`reason.label()` is the single user-facing vocabulary: `"Esc Esc"`,
`"new message"`, `"process exited"` — shared verbatim by the TUI, the web
panel, and headless output.

`at_ms` is Unix-epoch milliseconds on the payload, never the event-log
envelope timestamp: log compaction rewrites the `.jsonl` and drops every
envelope timestamp.

### 2. Reason parking, not threading

`HarnessError::Interrupted` stays a unit variant. Instead, each stop site
calls `RoundLifecycle::record_interrupt(reason)` at the moment it requests
the cancellation, and the unwinding round task reads it back with
`take_interrupt()` in `start_interactive_round`'s tail. One write, one
read — no producer of the error changes signature.

- `handlers_permission::interrupt` / `interrupt_side` park `user`.
- `start_interactive_round`'s predecessor-cancel, `supersede_for_session_switch`,
  `run_shell_command`'s predecessor-cancel, and the aside teardowns park
  `superseded`.

### 3. Persistence (schema v11)

`SessionData.round_interrupts: Vec<RoundInterrupt>` with
`#[serde(default, skip_serializing_if = "Vec::is_empty")]` (legacy
canonical JSON stays byte-identical, so existing checksums remain valid),
`SessionEvent::RoundInterruptRecorded` / `RoundInterruptsCleared`, and
`SessionStore::{record_round_interrupt, clear_round_interrupts,
round_interrupts}` mirroring the command-ledger shape. A duplicate guard
drops a repeated `(round, reason)` pair — the runtime can observe one stop
from two sites.

### 4. The two termination paths that run no code

- **Registry kill paths** (daemon shutdown drain, `KillSession`,
  `EndSession`) drop the driver future; the round task never runs its
  tail. `kill_session_with_hook_budget` therefore records `terminated`
  into the store *before* cancelling, when the monitor tracker says the
  session has active work.
- **Hard kills** (SIGKILL, panic, power loss) write nothing — the record
  is synthesized on the next load: `TokenLedger::restore_session` flips a
  persisted `InFlight` request to `Abandoned`, and the session driver
  emits one `terminated` record per abandoned round.

### 5. Projection on resume

The records ride the same channels as the command ledger:
`Wire::Welcome`, `AgentResponse::ConversationReplaced`, and
`SideViewOpened` each carry `round_interrupts` (`#[serde(default)]`, so
older daemons interoperate). The TUI merges the rebuilt marker rows at
their timestamp seams (`merge_round_interrupt_rows`, mirroring
`merge_command_rows`), and the web panel does the same in
`buildReplacedFeed`.

### 6. Transcript rendering (TUI)

A dedicated entry kind following the ADR-0111 universal Entry shape:

```text
▲ interrupted · 21:39          ← header: theme.warn() + BOLD, muted time tail
                               ← 1 blank row (TURN_HEADER_BODY_GAP_ROWS)
  round 3 · Esc Esc            ← body, TRANSCRIPT_BODY_LEADING_INDENT
```

`theme.warn()` matches the tool-step renderer's `ToolStatus::Interrupted`
tone, so every "a human (or the host) stopped this work" surface agrees.
The row is immutable and terminal (height-cache fast path), not
interactive, and acts as a turn-band group terminator like a notice.

### 7. Monitor

`RoundEvent::RoundInterrupted` folds to `SessionStatus::Interrupted` with
the reason label as the note — closing the pre-existing gap where a
phase-2/3 interrupt left the monitor row stuck on `Running`.

## Consequences

- A resumed session shows exactly which rounds stopped, why, and when —
  the "continue or not" decision is answerable at a glance.
- The supersede path is no longer invisible: a new message that kills a
  running round leaves `Interrupted · new message` at the seam.
- Process death is distinguishable from a user interrupt: `process
  exited` markers appear after a crash/kill even though no code ran.
- The model-visible context is unchanged — zero added tokens on any
  subsequent round, and the no-marker rationale of the interrupt-semantics
  doc still holds verbatim.
- Event-log size grows by one line per stopped round (bounded, append-only).
- `SessionEvent` gained two variants and the wire payloads gained one
  optional field each; all default-tolerant, so cross-version attach
  keeps working.
