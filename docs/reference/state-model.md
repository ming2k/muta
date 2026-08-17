# State and status model

neenee does not have one project-wide state machine. It has several
independent protocols whose subjects, owners, persistence, and transition
rules differ. Keeping those protocols separate prevents a display badge from
silently becoming execution authority.

## Classification rule

| Kind | Meaning | Transition enforcement |
|------|---------|------------------------|
| Lifecycle protocol | Controls whether work may start, continue, finish, or be superseded | Enforced by the owning component |
| Orthogonal UI axis | One independent input to presentation | Enforced locally; combines with other axes |
| Parked request protocol | A running round waiting for one external reply | Request id + one-shot settlement |
| Projection or label | Reports/classifies state owned elsewhere | No independent transition authority |

An enum is therefore not automatically a state machine. In particular,
`LoopStatus`, `ReviewStatus`, and todo status are useful vocabularies, but they
do not own the principal round lifecycle.

## Execution protocols

### Session round lifecycle

**Subject:** at most one active round for one session, whether that session is
the primary conversation or a `/btw` aside. Separate sessions own separate
lifecycle instances and may run concurrently — including several asides at
once (ADR-0103).

| Operation | Transition |
|-----------|------------|
| `begin` with no active round | inactive → active generation N |
| `begin` with an active round | generation N becomes stale; generation N+1 becomes active; caller cancels N |
| `finish(N)` while N is current | active N → inactive |
| `finish(N)` after supersession | no-op; the successor owns terminal cleanup |
| `cancel_current` | removes and cancels the live token without changing the generation |
| `supersede` | invalidates the current generation without installing a successor |

The generation guard, not a display enum, decides which round may emit
terminal cleanup. `LoopStatus` is only a UI projection:

- `idle`: no displayed work;
- `running`: a normal round.

Waiting for permission, user answers, or interactive input is not another
round lifecycle state. It is a parked-request overlay on the same running
round.

### ReAct turn

**Subject:** one model request and the tool work carried by its committed
response.

A streaming turn retains its prepared request, tool-call guard state,
completed-call checkpoint, accounting guard, hook scope, and steering inbox
across retryable provider failures. The important transition boundary is:

```text
preparing --> requesting
requesting --retryable failure, no unsafe replay--> retrying --> requesting
requesting --complete response--> committed
committed --tool calls--> executing --> recording --> next turn
committed --no tool calls--> round terminal
preparing/requesting/executing --interrupt--> interrupted
requesting/executing --terminal failure--> failed
```

Provider retry is allowed before a side effect. After a tool side effect has
committed, replay protection keeps that result authoritative and an unsafe
retry is terminal. A single turn may carry several parallel tool calls; those
calls are sibling operations inside the turn, not additional turns.

### Position and counter invariants

| Value | Scope | Rule |
|-------|-------|------|
| `round` | Session | Monotonic user-exchange counter; increments once when an admitted prompt opens a round |
| `turn` | Round | Model-request index; resets when a new round opens |
| `attempt` | Turn | Concrete provider attempt; increments on retry without creating another turn |

Internal loop indices are zero-based. User-facing transcript and token-report
labels are one-based. The runtime transports both `round` and `turn`; a UI must
not infer one from the other. Initial history and harness snapshots also carry
the persisted `round_counter`, because a compacted visible transcript cannot
reconstruct the session's absolute round number by counting messages.

### Provider request accounting

**Subject:** one concrete network attempt, keyed by session, actor, round,
turn, and attempt.

```text
in_flight --> completed | interrupted | failed
in_flight --crash recovery--> abandoned
```

Every state except `in_flight` is terminal. Token-source provenance
(`unknown`, `reported`, `estimated`) is a separate classification axis.
Retry creates another attempt under the same `(session, actor, round, turn)`;
it does not increment the turn.

## Parked request protocols

Permission, `ask_user`, and interactive stdin each store a one-shot sender
under a request id. Their shared settlement rule is exactly once:

```text
requested/parked --> replied | cancelled
```

- A permission reply carries once/always/reject.
- A question reply carries one answer array per question. An empty **outer**
  array is reserved for cancellation.
- An interactive-input reply carries text; interrupt/supersession rejects the
  waiter.

The TUI queue and modal are projections of these waiters. Closing a modal must
settle or deliberately retain its waiter; it must never only remove the UI
row.

### Question modal

**Subject:** one `ask_user` request containing one to five question pages.

The model keeps a page cursor plus per-page highlight, selections, and
**Other** text. `Enter` advances until the final page, then submits all pages.
`Shift+Tab` returns to the previous page. `Esc` settles the request as
cancelled and closes it.

```text
page N --Enter, not final--> page N+1
page N --Shift+Tab, N>1--> page N-1
final page --Enter--> submitted
any page --Esc/round cancellation--> cancelled
```

## Transcript step presentation

Each expandable transcript step combines three orthogonal axes:

| Axis | Examples | Owns |
|------|----------|------|
| Lifecycle | tool running/ok/failed/denied/cancelled | semantic accent |
| Disclosure | collapsed/expanded plus user pin | body visibility and weight |
| Interaction | idle/hovered/focused | transient weight lift |

These axes compose; none may mutate another. See
[Step state machine](tui/step-state.md) for the complete color and disclosure
tables.

## Status vocabularies that are not global FSMs

| Vocabulary | Subject | Semantics |
|------------|---------|-----------|
| MCP connection status | One configured server | Connecting may settle Connected or Failed; disable is a session policy; reconnect starts a new attempt |
| Todo status | One editable task-list item | Model-authored workflow label; updates may revise earlier labels, so no monotonic global transition graph is promised |
| Tool status | One rendered tool call | Projection of execution outcome; terminal values do not transition again |
| Modal identity | The currently open TUI overlay | Exclusive routing discriminant, not one lifecycle shared by all modal contents |
| OAuth/outbox progress | One frontend operation | Local workflow state scoped to that operation, not session execution state |

When adding a new status value, first name its subject and owner. Add a
transition rule only if the value controls behavior; otherwise document it as
a projection or classification.
