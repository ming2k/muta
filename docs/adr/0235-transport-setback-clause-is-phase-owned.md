# 0235. The transport-setback clause is owned by the transport-wait phase

- **Status:** Accepted
- **Date:** 2026-09-11
- **Scope:** `mutx` (activity bar clause: `crate::phase`, `app::SessionChrome`,
  the response translator in `lib.rs`, the mutation applier, the event-loop
  heartbeat, the renderer)
- **Amends:** ADR-0154 (the clause's *lifetime* — left unspecified there — is
  fixed here; the clause's vocabulary and slot exclusivity are unchanged). Its
  §3–5 *stall* clause (`TokenWatch`, `· no tokens Ns` / `· silent Ns`) is no
  longer shipped: that machinery was removed after this record and nothing
  derives it, so the transport clause is now the row's only annotation.

## Context

The activity bar renders one transient row above the input box. Beside its
master label it can show a **transport-setback clause** — `retry N/M (next in
4s)`, ADR-0154 — the one annotation allowed to ride next to the typed phase.

A user reported it stuck:

```text
 ● answering  retry 1/29 (running for 32s) [4m 45s]        Esc Esc interrupt
```

Every element of that row is true except the pairing. `answering` is the phase
the *retried request* moved the session into; `running for 32s` means the
backoff expired 32 s ago; `retry 1/29` describes an attempt that already
succeeded. The clause outlived its subject and stayed up — the model streaming
its whole answer — until something unrelated happened to retire it.

**Mechanism.** The clause was a latch: `App::provider_retry:
Option<ProviderRetryState>`, written once in the translator's
`RoundEvent::RetryScheduled` arm and **cleared by hand** at ten other arms of
the same match (`RoundCompleted`, `RoundInterrupted`, `Text`, `CommandResult`,
`StreamEnd`, `StreamDiscard`, `UnsentInput`, `ToolCall`, `HarnessState(!running)`,
`Error`). The arms that carry an ordinary successful answer were not among
them: `StreamStart` retires the *transcript* copy of the same fact
(`begin_stream` evicts the retry disclosure row) but sent no clause clear;
`StreamDelta` cleared nothing at all — it even passes `clear_retry: false` —
and neither did `StreamReasoningDelta`. So the countdown survived the stop
where it is most visible: the stream it was waiting for.

**Why the enumeration could not have been right.** The clause's only honest
terminator is "the attempt landed", and no wire event says that:
`RoundEvent::RetryResolved` is the nearest candidate and it is retired by
ADR-0194 (transient retries are in-flight execution detail, no post-round
notice). The activity vocabulary is open by construction — `Phase::Other`
exists so a new backend label never blanks the bar — so a lifecycle *derived*
by listing known events is one shipped event away from leaking, while the
phase it annotates is complete by definition: every fact the bar can display
moves the phase.

**Three symptoms, one disease.** The same latch was also:

- **Duplicated with a different lifetime.** The retry fact existed twice — the
  transcript disclosure row (retired at `StreamStart`) and the chrome latch
  (retired by the other list) — so the two representations of one domain fact
  disagreed on screen about whether the retry was over.
- **Global, after its neighbours went session-scoped.** `phase`, `responding`,
  the round/turn counters and `can_retry` were moved into `SessionChrome`
  precisely so a background `/btw` aside could not paint the primary's bar; the
  clause stayed an `App` field, so a background aside backing off against a
  rate-limited upstream painted its countdown onto the primary view.
- **Driving the frame clock.** `event_loop`'s `animating` predicate included
  `app.provider_retry.is_some()`, so a latch nobody could see still forced 10 fps
  redraws — the cost of the stale state outlived the display of it.

The diagnosis is not new: ADR-0092 already named it for the optimistic `queued`
label — *"the invariant lives as a comment that each handler must remember, and
every future handler reintroduces the bug"* — and repaired it by making the
resolution structural. This ADR is that repair for the clause.

## Decision

**The clause's lifetime is derived from the phase it annotates, and the phase's
writers are its owners.**

1. **One statement of the lifetime.** `phase::ends_transport_setback(phase)`
   is true for every phase except `Some(Phase::AwaitingModel)` — including
   `None`, the round being over. Only the transport-wait phase can host the
   claim "the request in flight did not land"; every other value is evidence
   that something did.

2. **The phase writers enforce it.** `SessionChrome::set_phase` (the
   session-scoped store, used by every `ChromeEdit`) and `App::set_phase` (the
   primary's mirror, used by every `AppMutation::SetPhase`) retire the clause
   they own and then write the phase. Every phase write in the process now goes
   through one of them, including `apply_chrome`, the interrupt path in the
   action layer, and `clear_responding`. The only other phase assignments are
   the view-swap mirrors (`apply_chrome` copying a viewed session's phase, the
   fresh-aside entry clearing the displayed surface): they write *what is on
   screen*, not a session's live phase, and both are documented as such.

3. **Producers publish, and cannot clear.**
   `AppMutation::SetProviderRetry(ProviderRetryState)` is no longer an
   `Option`: "clear the clause from here" is unrepresentable, so the reply to
   "which events end a retry?" is structural rather than a list. The ten
   hand-written clears are deleted. `RetryScheduled` routes the publication to
   the primary's mirror or, for an aside, to that aside's own chrome entry —
   each session's clause is retired by *its own* phase.

4. **Both readers derive from the same place.** The renderer reads the *viewed*
   session's clause (so an aside's countdown never appears on the primary bar),
   and the heartbeat asks whether *any* session has a live clause (so a detached
   aside's countdown still ticks, and the frame cost stops when the last one
   ends).

5. **`RoundEvent::Text` moves the phase.** A one-shot payload is the degenerate
   case of a delta stream and now asserts `Answering` exactly as `StreamStart`
   and `StreamDelta` do. The arm previously wrote no phase at all when the loop
   kept running, which was the last in-flight progress path that could not end
   the clause. The ADR-0092 §3 contract is preserved: the phase *fact* travels
   (as it already did for deltas), but only the authoritative idle snapshot
   collapses the bar.

### Invariants & Behavioral Boundaries

- **INV-1 — The clause is phase-owned.** The transport-setback clause is live if
  and only if its session's phase is `AwaitingModel`. No producer clears it:
  only a phase write may end it, through `phase::ends_transport_setback` applied
  by `SessionChrome::set_phase` / `App::set_phase`.
- **INV-2 — Producers publish, never clear.**
  `AppMutation::SetProviderRetry` carries a setback, never an absence. A "list of
  events that mean the retry is over" (in the translator, the applier, or a
  future frontend) must not be reintroduced; this is what the deleted ten-arm
  clear list was.
- **INV-3 — The clause is session-scoped.** It is displayed from the *viewed*
  session's chrome only, and a session's clause is retired only by that
  session's phase writes. A background session's retry never appears on another
  view's bar (`App::provider_retry` is the primary's slot).
- **INV-4 — Display and animation read one source.** The renderer and the
  frame-clock `animating` predicate both derive liveness from the same state
  (`App::has_live_transport_setback`); no clause may animate without being
  displayable in some view, and no displayable one may be frozen.
- **INV-5 — Every displayable fact moves the phase, and only idle collapses the
  bar.** Content that the bar can show (a one-shot `Text` payload included)
  writes `Answering` like a delta; only the authoritative idle snapshot / round
  end clears the phase (ADR-0092 §3 contract).

### Why "the phase left the transport-wait phase" is a complete terminator

The rule would be worthless if an attempt could land without moving the phase,
so the completeness is argued here rather than discovered later:

| What the retried attempt produced | Event | Phase written | Clause |
|---|---|---|---|
| Visible text, streamed | `StreamStart` → `StreamDelta(…)` | `Answering` | Retired at `StreamStart` |
| Visible text, one-shot payload | `Text` | `Answering` (decision 5) | Retired |
| Reasoning only | `StreamReasoningDelta` | `Reasoning` | Retired |
| A tool call | `ToolCall` | `Tool(verb)` | Retired |
| A transient failure | `RetryScheduled` again | `AwaitingModel` | Replaced — still one live attempt, still counted down |
| A terminal failure | `Error` | `None` | Retired |
| Nothing at all (empty completion) | — | The round has nothing to continue with and ends, so the terminal idle `HarnessState` writes `None` | Retired |

The one transition that stays inside `AwaitingModel` — a new logical turn,
`TurnStarted` — cannot follow a retried attempt without one of the outcomes
above: a turn continues only through a tool call (`ToolCall` → `Tool(_)`) or the
round ends. The daemon also makes the identity explicit: a transport retry
reuses its prepared request (`rounds.rs`: *"A transient retry deliberately
skips this block: the already-prepared request is the checkpoint"*), so
`TurnStarted` is emitted once per logical turn and never per network attempt.
Naming the phase is therefore enough — no attempt id, no position stamp, no
timer.

## Alternatives considered

- **Add the missing clears** (`SetProviderRetry(None)` in `StreamStart`,
  `StreamDelta`, `StreamReasoningDelta`). Rejected: it is the status quo's
  failure mode — the invariant stays a comment that each arm must remember, and
  the next progress event to land without a clear re-breaks the clause. This is
  ADR-0092's rejected alternative, again.
- **Expire the clause on a clock** (a watchdog, or a render-side
  "stale after N seconds" heuristic). Rejected: a timer cannot distinguish "the
  attempt landed" from "the attempt is slow" — the very distinction the clause
  exists to make. ADR-0092 rejected the same shape for `queued`.
- **Gate the clause at render time and leave the latch uncleared.** Rejected:
  it hides the symptom while keeping the state — the heartbeat still spins for
  it, and it can *resurrect* on the next `AwaitingModel` write from an
  unrelated turn.
- **Keep one global slot and tag it with its session id.** Rejected as the
  category error ADR-0232 rejected: a shared slot whose identity is
  reconstructed from the payload aliases as soon as two sessions are in flight,
  and "the most recent setback" is not the viewed session's setback.
- **Retire the clause on `RoundEvent::RetryResolved`.** Rejected: that event is
  retired by ADR-0194 and, when it existed, fired at the round's terminal path —
  after the recovery, not at it.
- **Move the clause wholly into `SessionChrome` and delete the App mirror.**
  Rejected: the primary's map-entry phase does not receive every primary phase
  fact (only the chrome-edited ones do), so retirement would depend on the
  translator remembering twin writes. The clause must live in the store whose
  phase retires it — for the primary that is the App mirror, which
  `AppMutation::SetPhase` writes on every phase fact.

## Consequences

**Positive.**

- The clause cannot outlive the attempt it describes, on any path — including
  events this client has not learned yet: if a fact moves the phase, it retires
  the clause; if it does not move the phase, it cannot be displayed either.
- The transcript disclosure row and the chrome clause now agree by
  construction: both are retired by evidence that the stream started, and the
  chrome one no longer depends on an arm remembering to say so.
- A background aside's countdown is its own; leaving and re-entering a view has
  nothing to restore, because the clause is not a displayed-mirror slot.
- The 10 fps heartbeat ends with the last live clause instead of running until
  some other arm fires.
- A non-streaming route's one-shot reply now reads `answering` rather than
  leaving `waiting for model` up after the model has answered.

**Negative / neutral.**

- A primary phase fact must be written to both stores (`SetPhase` for the App
  mirror, the matching `ChromeEdit` for the session entry). This twin-write
  discipline predates this ADR — `phase` already depends on it, and the `Text`
  arm now follows `StreamDelta`'s existing pattern (`chrome!` unconditional,
  `SetPhase` gated on `routes_to_side`) — but the clause inherits it.
- `App::provider_retry` and `SessionChrome::transport_setback` are therefore
  asymmetric: the primary's clause is not swapped or parked by a view change
  (`apply_chrome` copies the phase, not the clause). Forced by the store
  topology above, and documented at both fields.
- The translator keeps a listener-local `retry` mirror, now purely to phrase
  `Exhausted N retry attempts — …` for the terminal `Error` arm (the wire error
  carries no retry count). It is bookkeeping, not state: it feeds one notice's
  wording and cannot reach the bar. Its `retry = None` clears are therefore
  deliberately *not* the clause's lifetime, and are documented as such at the
  declaration.

## Verification

- `tests::transport_setback::*` drives the applier with the exact mutation
  sequences the translator emits: a started stream retires the clause, every
  progress phase retires it, a one-shot `Text` payload retires it, a round end
  retires it, a second retry replaces it without a gap, an aside's setback never
  reaches the primary bar and is retired by the aside's own progress.
- `phase::tests::only_the_transport_wait_phase_hosts_a_setback_clause` pins the
  lifetime rule itself (the rule's single home).
- `chrome::tests` keeps covering the rendered clause (full, compact, dropped
  under width pressure).
- `cargo nextest run -p mutx` — 1172 tests green.

## References

- [ADR-0092](0092-guaranteed-activity-resolution.md) — the same diagnosis
  (per-handler courtesy vs. structural resolution) for the optimistic `queued`
  label; its §3 contract is preserved by decision 5.
- [ADR-0154](0154-activity-bar-one-dot-stall-only-clause.md) — one dot, the
  stall-only clause, and the clause slot's exclusivity; this ADR supplies the
  lifetime it left open.
- [ADR-0194](0194-first-class-execution-telemetry-and-retry-resolution-eradication.md)
  — retries are in-flight detail and `RetryResolved` is retired, which is why
  "the attempt landed" must be derived.
- [ADR-0232](0232-attempt-owned-transport-telemetry.md) — attempt-owned state
  over a shared slot with a reconstructed identity.
- `docs/reference/tui/activity-bar.md` — the clause's lifetime, documented for
  users of the surface.
- `apps/tui/crates/mutx/src/phase.rs` — the fold's design rules, rule 4.
