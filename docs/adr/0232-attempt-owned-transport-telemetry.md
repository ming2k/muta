# 0232. Attempt-owned transport telemetry replaces the shared timings slot

- **Status:** Accepted
- **Date:** 2026-09-11
- **Scope:** `muta-contracts` (request contract, capability), `muta-llm-client` (egress seam, request builder, four protocol adapters), `muta-agent` (request accounting), `mutx` (latency timeline)
- **Amends:** ADR-0200 (the delivery mechanism for transport timings, and only that)

## Context

ADR-0200 committed the project to one promise: every displayed timing is a pure
function of a recorded trace. It named the delivery mechanism in the same
breath — the egress derives `TransportTimings` from its trace and hands them up
"through `muta_contracts::TransportTimings` and `Provider::take_transport_timings`".

That mechanism was a **single shared slot**: one
`Arc<Mutex<Option<TransportTimings>>>` per transport, written by the transport,
read once by whoever asked. It had two defects, and the first hid the second.

**Defect one: nothing wrote it.** The transport recorded a full trace for every
request — DNS, TCP, TLS, the request write, the response head, `TCP_INFO` — and
`derive()` existed to turn a trace into timings, but no code path ever called it
on the way out. The slot was created, threaded through the egress's body stream,
and never written. Every attempt in the wild therefore reported no connection
phase at all (22 962 recorded attempts, zero `dns_us`/`tcp_us`/`tls_us`/`rtt_us`
values), with three user-visible consequences: the latency timeline could never
show a handshake; it labelled every attempt — including cold starts — as
`reused warm pool connection`, because an absent field was read as evidence of
reuse; and the retransmit count rendered as a confident `0` for a socket nothing
had ever sampled.

**Defect two: the slot identifies nothing.** A transport is built once per
provider, and subagents share their parent's provider by `Arc` clone. So two
attempts overlapping on one provider contend for one slot: the earlier caller can
claim the later attempt's numbers, and the later caller can find the slot empty
and conclude it observed nothing. Sequential use is unaffected — which is why
the defect stayed invisible while the slot stayed empty, and would have become
reachable the moment defect one was fixed.

The underlying error is a category mistake: **"the most recent sample" is not an
attempt identity.** A pull interface cannot express "the timings for *my* call",
so any repair that keeps the pull shape has to reconstruct identity from
somewhere else — a key on the transport, a lookup table, a convention about
ordering — and every such reconstruction has an aliasing case that fails
silently.

## Decision

**Transport telemetry is owned by the attempt that caused it, and travels with
the request that produced it.** The shared slot, and the whole pull interface
built on it, are deleted.

A `TransportTelemetry` handle is created by the attempt's owner — the request
accounting guard, at the same moment it allocates the attempt's ledger record —
and carried on the request itself:

```text
  guard (per attempt)            transport                 guard
  creates handle  ──►  ModelRequest.transport_telemetry
                                │
                                ├─► RequestBuilder ─► RequestParts
                                │                       │
                                │              egress derives the trace
                                │              and publishes into the handle
                                ▼                       │
  reads handle after the call (success or failure) ◄────┘
```

- **`ModelRequest.transport_telemetry`** is runtime-only (`#[serde(skip)]`,
  never serialized, never persisted, never on the wire), exactly as
  `ModelRequest.turn_context` already is: both are per-call state that exists
  only inside the process assembling and issuing the call.
- **The owner stamps it per attempt.** A retry reuses `round.pending_request`,
  so the agent stamps a freshly created handle onto the per-attempt clone it
  hands the provider. Each dispatch gets its own; no two attempts can share one.
- **Push, not pull.** The transport writes into the handle it was handed. No
  reader has to ask which attempt a sample belongs to, because the only reader
  is the owner.
- **Absence stays honest.** A handle with no reading yields `None`, and the
  ledger's `TransportObservation` distinguishes *the transport observed a pooled
  socket* from *nothing observed the socket*. Neither is inferred from an absent
  field (the invariant ADR-0200 stated and this record extends to the seam).
- **Failure paths report too.** The handle is created before dispatch and read
  after the call returns, so an attempt that never opened a stream (a refused
  connection, a DNS failure) still reports the phases it paid — the very
  information a user diagnoses such a failure with.

### Invariants & Behavioral Boundaries

1. **Telemetry travels with its request.** A timing reaches a record only by
   travelling on the request that produced it. There is no lookup, no key, no
   "most recent".
2. **No shared mutable telemetry state.** No transport, provider, client, or
   egress holds a telemetry slot that more than one attempt can reach. This is
   what makes concurrent attempts on a shared provider non-aliasing by
   construction rather than by convention.
3. **`Provider::take_transport_timings` no longer exists.** The provider trait
   carries no telemetry accessor: a provider's job is to execute a request, and
   the timings belong to the caller that issued it.
4. **The handle is created per attempt, before dispatch.** A retry must stamp a
   fresh handle; reusing one across attempts is the defect this record removes.
5. **`dispatch_at` never leaves the process.** The anchor is `#[serde(skip)]` and
   `#[ts(skip)]`; a monotonic clock instant is meaningful only to the process
   that read it. Offsets measured from it are re-anchored or refused, never
   compared across processes.
6. **No timing may be rendered without its verdict.** The ledger reads a
   `TransportObservation`; a rendered connection row or retransmit count
   requires the transport to have been watching.

## Alternatives considered

- **Keep the pull interface and key the slot by `RequestUsageKey`.** This was
  the first considered repair, and it resolves the aliasing — at a cost: the
  ledger's key must travel into the transport and its adapters (a layering
  inversion: the transport would learn the ledger's vocabulary), the map needs
  an eviction policy because attempts outlive no bound the transport knows, and
  it reconstructs identity instead of carrying it. Rejected: a key that is
  reused or mistyped fails as silent cross-attribution, which is the same
  failure mode as the defect, only harder to see.
- **Return the handle alongside the result.** The provider call would yield
  `(stream, handle)` rather than `stream`. Rejected: the pre-stream failure path
  returns `Err` and would drop exactly the timings that matter most — the phases
  paid before a connection failed. Verified by test, not assumed: a refused
  connection reports DNS paid, TCP incomplete, and no connection instant.
- **Add a parameter to `Provider::stream_chat_events`.** Rejected: the trait has
  28 implementors (4 production adapters, the rest test doubles and proxies), so
  the parameter is a large mechanical change for information that *already*
  travels on the request it describes. Passing it twice invites the two copies
  to disagree.
- **Hang the handle on `ProviderTurnContext`.** Rejected: `turn_context` is
  created per *round* and shared across that round's retries. Per-attempt state
  placed there would alias across attempts on the same turn — reintroducing the
  exact bug.
- **Make timings a field of `ProviderStreamEvent::Completed`.** Elegant for the
  success path (the timings arrive with the stream that produced them), but a
  failed attempt emits no `Completed`, and `ProviderError` is a serialized
  contract type that should not grow a clock-carrying runtime field to
  compensate. Rejected as incomplete rather than wrong.
- **Leave the aliasing to be fixed when it is observed.** Rejected: fixing
  defect one makes it reachable, and its failure mode is a plausible-looking
  number attributed to the wrong attempt — the class of error this project's
  telemetry exists to eliminate.

## Consequences

- **Positive:** the aliasing case is unrepresentable, not merely avoided; an
  entire pull protocol (`Provider::take_transport_timings`,
  `Client::take_transport_timings`, `Egress::timings_slot`, `TimingsSlot`) is
  deleted rather than repaired; failures and abandoned streams report as
  naturally as successes, because all three read the same owner-held handle.
- **Negative:** the request contract grows a runtime-only field whose purpose is
  a caller's concern rather than the provider's, so a provider adapter must pass
  it through (`RequestBuilder::with_telemetry`) even though it cannot interpret
  it. Accepted: the alternative is identity reconstruction, and the pass-through
  is one line per adapter, checked by the compiler when the field is added.
- **Neutral:** the ledger's persisted shape is unchanged by this record; the
  `TransportObservation` and `connected_us` fields it builds on were added by the
  same change set and are documented in the changelog.
- **Superseded in part:** ADR-0200's "Transport timings reach the ledger"
  paragraph named `Provider::take_transport_timings` as the mechanism. The
  promise stands; the mechanism is replaced here. ADR-0200's decision record is
  otherwise untouched and remains the authority on the transport, the trace
  model, and the tap levels.

## References

- ADR-0200 — the owned egress path, the trace model, and the promise this record
  keeps with a different mechanism.
- ADR-0210 — extraction of the transport stack as `netune`; `derive()` and the
  `Validity`/`Reason` vocabulary live there.
- ADR-0151 — per-attempt client-observed performance telemetry, and the
  single-rate doctrine the ledger's timing fields serve.
