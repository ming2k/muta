# 0092. Guaranteed activity resolution for non-round requests

- **Status:** Accepted
- **Date:** 2026-08-07
- **Related:** [ADR-0008](0008-single-breathing-anchor.md) (the activity bar is
  the single liveness anchor), [ADR-0078](0078-round-lifecycle-type.md)
  (`RoundLifecycle` + `LoopStatus` as the authoritative round state),
  [ADR-0088](0088-command-acknowledgment-toast-notices.md) (the migration that
  exposed the defect)

## Context

The TUI's activity bar is a projection of two state fragments:

1. **Authoritative** — the harness's `LoopStatus`, emitted by the driver as
   `RoundEvent::HarnessState` at round boundaries. The round task
   (`start_interactive_round`'s spawned closure in
   `crates/neenee-agent/src/orchestration.rs`) **always** emits a terminal
   `HarnessState(Idle)` when it finishes, errors, or is interrupted — the round
   lifecycle is self-closing by construction.
2. **Optimistic** — `is_responding` + `activity_status`, invented by the TUI at
   dispatch time. `SendChat` / `SendSlash` / `SendShell`
   (`crates/neenee-cli/src/tui/event_loop.rs`) set `is_responding = true` and
   paint `activity_status = "queued"` before the request reaches the driver.

The optimistic fragment had **no guaranteed resolution**. It was resolved only
when the driver *happened* to emit one of a set of terminal events
(`RoundEvent::Text`, `RoundEvent::Error`, `HarnessState(Idle)`) for the
request. Chat rounds closed themselves (the round task's terminal idle
snapshot), but **control-plane requests did not own the lifecycle**, so their
resolution depended on the individual handler remembering to emit a terminal
event.

ADR-0088 made this bug class visible: it migrated `/autopilot`'s reply from
`RoundEvent::Text` (which *incidentally* cleared the activity surface) to a
toast `RoundEvent::Notice` (which does not). The handler then emitted **no**
terminal event at all, so dispatching `/autopilot on` while the harness was
idle left the activity bar stuck on `● queued (Esc Esc to interrupt)` forever —
until the next real round happened to start and clear it. The bar claimed a
round was pending and interruptible when the command had already completed
synchronously.

The same asymmetric design allowed two adjacent defects:

- `SendSlash` / `SendShell` painted `"queued"` **over a live round's** activity
  label ("grep", "thinking", …), even though a control-plane command did not
  start or replace the round.
- `RoundEvent::Text` cleared the activity surface **even mid-round**, tearing
  down a running round's bar for a slash reply that had nothing to do with the
  round. The lifecycle side effect was coupled to a *content* event, which is
  why migrating a handler from `Text` to `Notice` silently dropped it.

## Decision

Establish the invariant **every dispatched request lands the harness back in
its authoritative state**, enforced structurally rather than by per-handler
courtesy, and stop coupling lifecycle resolution to content events.

### 1. Driver: reconcile control-plane dispatches (the structural invariant)

In `SessionDriver::run` (`crates/neenee-transport/src/session_driver.rs`), after
the request `match` completes and **before** the post-dispatch projection,
re-publish the harness state for every request that does not own the round
lifecycle:

- Requests classified `round_owned` (`Chat`, `ChatToSession`,
  `InsertUserInput`, `CancelInsertedInput`, `ShellCommand`) close their own
  resolution via the round task's terminal `HarnessState(Idle)` — no reconcile.
- Every other request (slash commands, provider/session/tool/MCP toggles,
  queries, layout updates, permission/question/input replies, …) is reconciled:
  if `lifecycle.is_running()` is false, emit
  `send_harness_state(…, LoopStatus::Idle)`, which the TUI's existing
  `HarnessState` handler maps to "collapse the activity bar". When a round is
  live the reconcile is a no-op — the round's own events own the display, and
  re-emitting a running snapshot would reset the TUI's round timer/turn
  counters.

This makes the terminal emit a **structural guarantee** instead of a
per-handler obligation. A future handler that emits no terminal event (or a
future frontend request type) can no longer strand the optimistic `"queued"`
state, because the driver always reconciles back to the authoritative snapshot.
`round_owned_request` is a pure classifier with unit tests.

### 2. TUI: never paint optimistic activity over a running round

`SendSlash` and `SendShell` set `is_responding` / `"queued"` only when the
viewed session is **not** already in `running_sessions`. A live round owns the
activity surface; a control-plane command must not fabricate a `"queued"`
label over it, and must not be able to leave one behind. The `/serve`
sub-branch (which resolves entirely in the TUI, never reaching the driver)
retires the optimistic state itself — clearing both `is_responding` and
`activity_status` — and only when it painted them.

### 3. TUI: `Text` is content, not a lifecycle signal

`RoundEvent::Text` no longer clears `is_responding` / `activity_status`
unconditionally. It collapses the surface only when the harness's `LoopStatus`
is actually idle. A slash reply delivered mid-round cannot tear down the
running round's bar; the round's terminal `HarnessState(Idle)` (or the driver's
reconcile) is what retires the surface when the harness truly goes idle. This
removes the hidden coupling that ADR-0088 tripped over: content events no
longer carry lifecycle side effects.

### 4. Symmetric refusal contract for `AgentRequest::Chat`

`handlers_chat::chat`'s no-provider refusal emitted only a top-level
`AgentResponse::Error`, which the TUI surfaces as a notice but never uses to
clear the optimistic state. It now reuses the same `RoundEvent::Error` + idle
`HarnessState` refusal contract as every other round-entry path
(`refuse_if_no_provider`, `crates/neenee-transport/src/side.rs`), so a refused
send cannot strand `"queued"` either.

## Alternatives considered

- **Per-handler terminal events** (patch `/autopilot` to emit
  `HarnessState(Idle)` and audit every other handler). Rejected: it is the
  status quo's *failure mode* — the invariant lives as a comment that each
  handler must remember, and every future handler reintroduces the bug. The
  driver-level reconcile makes the invariant structural.
- **Emit `HarnessState(Idle)` after *every* dispatch, including round-owned
  ones.** Rejected: duplicate terminal snapshots for chat/shell rounds, and —
  more importantly — re-emitting a *running* snapshot would reset the TUI's
  round timer and turn counter mid-round, so the guard on
  `!lifecycle.is_running()` is load-bearing.
- **TUI-side watchdog** (a timeout that clears a stuck `"queued"`). Rejected:
  a timer cannot distinguish "slow round" from "orphaned control-plane state",
  so it would hide genuine long-running work behind a misleading collapse.
- **Request/response correlation** (pair every `AgentRequest` with its terminal
  `AgentResponse`). Rejected as over-engineering for this defect: the protocol
  is already session-scoped and ordered, and the driver reconcile achieves the
  guarantee without threading correlation ids through every handler.

## Consequences

**Positive.**

- `/autopilot on|off` while idle now collapses the activity bar as soon as the
  command completes: `queued` → `HarnessState(Idle)` → bar hidden. The
  confirmation toast and the `autopilot` status-bar badge still surface the
  result.
- The whole class is closed: any control-plane request (current or future) is
  reconciled to the authoritative harness state after dispatch.
- Slash commands and `!` shell passthroughs dispatched mid-round no longer
  clobber or tear down the running round's activity display.
- `RoundEvent::Text` / `Error` no longer carry hidden lifecycle side effects;
  content and lifecycle are decoupled.

**Negative.**

- One extra, idempotent `HarnessState(Idle)` event per control-plane dispatch
  while the harness is idle (the reconcile). The TUI's idle-snapshot handler is
  already idempotent (clearing an already-cleared surface), so this is noise,
  not work.

**Neutral.**

- Round-owned paths (`Chat`, `ChatToSession`, `ShellCommand`,
  `InsertUserInput`) are untouched: their resolution continues to come from the
  round task's terminal `HarnessState(Idle)`.

## Verification

- `round_owned_request` unit tests classify every `AgentRequest` variant
  (`crates/neenee-transport/src/session_driver.rs`).
- Manual: run `/autopilot on` with an idle harness — the activity bar shows
  `queued` only transiently, then collapses; the toast and `autopilot` badge
  remain. Run `/autopilot on` mid-round — the round's activity label is
  untouched.
- Full workspace test suite (`cargo test --workspace`) passes.

## References

- [ADR-0008](0008-single-breathing-anchor.md) — the activity bar as the single
  liveness anchor.
- [ADR-0078](0078-round-lifecycle-type.md) — `RoundLifecycle` / `LoopStatus` as
  the authoritative round state.
- [ADR-0088](0088-command-acknowledgment-toast-notices.md) — the migration that
  exposed the unguaranteed resolution.
- `crates/neenee-transport/src/session_driver.rs` — the driver reconcile.
- `crates/neenee-transport/src/side.rs` — `refuse_if_no_provider` refusal
  contract.
- `crates/neenee-cli/src/tui/mod.rs`, `crates/neenee-cli/src/tui/event_loop.rs`
  — the TUI activity surface.
