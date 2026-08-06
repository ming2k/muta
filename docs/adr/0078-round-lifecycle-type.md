# ADR-0078: Round lifecycle in one type; typed `LoopStatus` badge

- **Status:** Accepted
- **Date:** 2026-07-23

## Context

At most one round (the user-perceived unit, ADR-0047) may be active per
session, and a newer round supersedes the previous one. That protocol —
a cancellation-token slot plus a generation counter — was never a named
thing. It was threaded as two bare `Arc`s
(`Arc<AsyncRwLock<Option<CancellationToken>>>` + `Arc<AtomicU64>`) through
roughly ten signatures across `neenee-transport` and the binary
(`SessionDriver`, chat/shell/slash/permission/session handlers, `/btw`
side sessions, `SlashContext`), and its begin / supersede / cancel / finish
steps were re-implemented inline at nine sites:
`start_interactive_round`, `start_pursuit`, `run_shell_command`, the five
session-switch slash arms (`/resume`, `/session fork|open|resume|new`),
`Interrupt`, and `exit_side_view`.

The copies carried load-bearing distinctions that existed only in comments:

- **Interrupt does not bump the generation**, so the unwinding round still
  emits its own `[Interrupted]` cleanup; session switches do bump it, so the
  stale round's cleanup is suppressed and the switch handler owns the
  terminal events.
- The `!` shell path rejects pending permissions but deliberately not
  pending inputs.
- Permission prompts are rejected and `PermissionsCleared` is sent *before*
  the predecessor token is cancelled, so parked replies resolve first.

Meanwhile `HarnessSnapshot.loop_status` was a stringly-typed
`"idle" | "running" | "pursue"` matched against string literals on both
sides of the wire.

ADR-0071 deferred extracting an agent kernel (no second consumer). The
duplication above is the kind of pain a kernel extraction promises to fix —
but it is a representation problem, not a packaging problem, and it can be
fixed in place.

## Decision

Consolidate the protocol into one type without introducing a runner crate
or a general state machine:

1. **`RoundLifecycle` in `neenee-agent`** owns the token slot and
   generation counter for one session (primary or `/btw` side). Its API
   encodes the formerly comment-only semantics: `begin()` (bump generation,
   install fresh token, return the superseded predecessor),
   `is_current(generation)`, `finish(generation)` (release the slot only if
   still current), `supersede()` (bump only), `cancel_current()` (take and
   cancel, no bump), `is_running()`. Every driver site above delegates to
   it. The protocol itself is deliberately binary: no active round, or an
   active round identified by a generation.
2. **`LoopStatus` enum in `neenee-core`** replaces the stringly-typed
   `HarnessSnapshot.loop_status`. Variants `Idle` / `Running` / `Pursue`
   serialize with `rename_all = "lowercase"`, so the wire format is
   byte-identical. `LoopStatus` is documented as a display-level badge for
   the activity bar, not the protocol state: `Pursue` is a running round
   with the pursuit stop-gate armed (ADR-0031), and awaiting-permission /
   awaiting-input remain overlays derived from the parked-request tables
   (`ParentStatus`), not status values.

Explicit non-goals: no runner crate, no formal round state machine, and no
`Paused` / `AwaitingPermission` variant (see alternatives).

## Alternatives considered

- **Extract a runner crate owning a full execution state machine.**
  Rejected per ADR-0071: there is still exactly one consumer chain (TUI,
  `/serve`, and `/repeat` all feed the same `req_tx`), and the previously
  scaffolded multi-session registry (ADR-0037/0054) was removed for sitting
  idle. A crate boundary also fixes nothing by itself — the scattered
  fields would move, not consolidate. Revisit only when a second driver
  (headless mode, daemon, SDK embedding) creates consumer pull.
- **Model a formal state machine including `Paused`/`AwaitingPermission`.**
  Rejected. Permission waiting is already ground-truthed by the parked
  oneshot tables in `PermissionStore`; a parallel enum value would be a
  second source of truth that must be flipped on every request/reply. It
  also has no lifecycle meaning (interrupt behaves identically whether a
  prompt is parked), the TUI already composites it as an overlay
  (`ParentStatus::NeedsApproval`, the activity bar's "awaiting
  permission"), and "pause" implies a user-level pause/resume this system
  deliberately does not have. ADR-0010 removed exactly this kind of status
  machine.
- **Drop `Pursue` from `LoopStatus` and derive the badge from pursuit
  state.** Rejected for now: the pursuit-armed flag is not part of
  `HarnessSnapshot`, so the TUI could not distinguish "pursuit armed and
  running" from "objective set but idle" without a wire addition — churn
  for no consumer benefit. The badge stays explicit and cheap.
- **Leave the duplication.** Rejected: nine copies of a concurrency
  protocol whose safety depends on ordering subtleties documented only in
  prose is where the next race condition comes from.

## Consequences

- One choke point owns the supersede/cancel/finish ordering; the interrupt
  vs. session-switch distinction is now visible in the API
  (`cancel_current` vs. `supersede` + `cancel_current`) instead of in
  comments.
- `loop_status` mismatches become compile errors on both sides of the wire;
  serde keeps `"idle" | "running" | "pursue"`, so `/serve` clients are
  unaffected. `docs/reference/server.asyncapi.yaml` narrows the field to
  the three enum values.
- The vocabulary follows ADR-0047: the type guards a **round** (the
  user-perceived unit). `docs/reference/glossary.md` still used the inverse,
  pre-ADR-0047 convention and is corrected alongside this ADR;
  `docs/explanation/agent-design/rounds-and-turns.md` has the same
  inversion throughout and still needs its own pass.
- Neutral: `LoopStatus` keeps three display values rather than collapsing
  to the protocol's binary view — a deliberate badge/protocol split, not an
  oversight.
- If a second driver ever appears, `RoundLifecycle` plus the existing
  `execute_round` entry point is the natural extraction seed — consumer
  pull, not architecture push (ADR-0071).

## References

- [ADR-0009](0009-uncapped-agentic-loop.md),
  ADR-0010 — event-driven loop over hard caps and status machines.
- [ADR-0017](0017-side-conversations.md) — `/btw` side sessions peer the
  protocol.
- ADR-0031 — pursuit is one round with an armed stop-gate, not a loop of
  turns.
- [ADR-0047](0047-round-contains-turn-vocabulary.md) — round / turn
  vocabulary this ADR's naming follows.
- [ADR-0055](0055-session-scoped-request-lifecycle-accounting.md) —
  session/actor/round/turn/attempt accounting vocabulary.
- [ADR-0071](0071-defer-kernel-split-and-backport-strictness.md) — the
  kernel-split deferral this ADR respects.
