# 0055. Session-scoped request lifecycle accounting

- **Status:** Accepted
- **Date:** 2026-07-12

## Context

ADR-0044 established provider-reported usage with a local estimator fallback,
but its ledger accumulated only completed requests under a provider/model key.
That shape conflated two different quantities:

- current context, which is the non-additive size of the next provider input;
- request usage, which is additive input/output consumption per network attempt.

It also had no identity or terminal state for an individual attempt. An
interrupt before the terminal usage event recorded zero, retries before the
successful attempt disappeared, a provider switch could change attribution at
booking time, and one process-global ledger mixed opened sessions while losing
all data on restart. Envoy requests were folded into a parent result total but
were not independently attributable.

The UI exposed the mismatch most clearly before the first request: the context
meter had a pre-wire estimate while the completed-request ledger was empty.

## Decision

Separate current context from request usage and model every provider call as a
lifecycle record.

1. Keep current context as a replaceable session snapshot derived from the
   exact next-request projection. Recompute it after request preparation and
   after the final committed history changes. Provider usage never replaces
   this state directly.
2. Identify every network attempt by session, actor, round, turn, and attempt.
   Follow ADR-0047 vocabulary: a round is the user-perceived exchange; a turn
   is one model request inside that round. The actor distinguishes the
   principal from individual envoy calls.
3. Move each request through `in_flight` to one terminal state: `completed`,
   `interrupted`, `failed`, or `abandoned`. A persisted `in_flight` record is
   restored as `abandoned` after a crash.
4. Upsert terminal information into the same keyed record. Reported provider
   usage can upgrade an estimate; a later estimate cannot downgrade reported
   usage. Duplicate terminal events are idempotent.
5. When provider usage is absent, estimate both the pre-wire prompt and the
   observed completion. Do not compare a response-only estimate with a
   reported prompt-plus-completion total.
6. Treat retries as distinct attempts because every request that reached the
   provider may be billable. Preserve failed and interrupted attempts rather
   than showing only the eventual success.
7. Persist request records inside the owning session's event-sourced state.
   Opening or resuming a session restores only its records. A fork inherits
   context but starts a new request ledger, so historical billing is not
   duplicated across branches.
8. Route context snapshots and request reports by session id. Primary and side
   sessions may run concurrently without overwriting each other's display.
9. Capture provider and model before dispatch. A later provider switch cannot
   reattribute an already-started request.
10. Present the report as current context plus a provider/model summary whose
    detail groups attempts by round and turn, including lifecycle state and
    reported/estimated provenance.

ADR-0044 remains authoritative for provider usage parsing and estimator
priority. This decision refines its ledger identity, persistence, fallback
scope, and UI semantics.

## Alternatives considered

### Update only after a completed round

Rejected because interruption, timeout, retry, and crash paths have no
completed usage event. The current context would also remain stale after an
unsend or partial-response discard.

### Add interrupted totals directly to provider/model counters

Rejected because counters cannot express idempotent backfill, attempt state,
or a later authoritative usage upgrade. They also cannot distinguish a retry
from an accidental duplicate booking.

### Use provider total usage as current context

Rejected because billed output can include hidden reasoning or discarded
generation that is not part of the next request. Tool results, compaction, and
model-specific request shaping can also change the next input independently of
the previous request total.

### Copy request history into forked sessions

Rejected because the same provider requests would then appear as consumption
in multiple branches. Forks inherit model-visible context, not past billing.

## Consequences

**Positive.** Current context remains timely after interruption and unsend.
Every retry and nested envoy request is attributable. Reported and estimated
totals are comparable. Session reports survive restart and remain isolated
across primary, side, opened, and forked sessions.

**Negative.** Session snapshots and event logs grow with request attempts.
In-flight persistence adds a small write near request dispatch. Interrupted
usage without a provider terminal event remains an estimate and may differ
from the provider invoice.

**Neutral.** Existing provider adapters and their usage priority chain do not
change. Legacy aggregate recorder APIs remain compatible but production
request accounting uses lifecycle records.

## References

- [ADR-0044](0044-layered-token-accounting.md)
- [ADR-0047](0047-round-contains-turn-vocabulary.md)
- [ADR-0048](0048-session-as-single-source-of-truth.md)
- [Token accounting](../explanation/agent-design/token-accounting.md)
- [Interrupt semantics](../explanation/interrupt-semantics.md)
