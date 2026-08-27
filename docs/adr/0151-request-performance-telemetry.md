# 0151. Per-attempt client-observed performance telemetry and the dedicated Performance report

- **Status:** Accepted
- **Date:** 2026-09-10

## Context

Throughput presentation was coupled to context accounting in two ways that both
turned out to be wrong:

1. **One denominator, many meanings.** The Context Usage modal's "Output rate"
   row and per-round `TPS` column divided output tokens by `generation_ms`, which
   spans request dispatch → validated response on the client. That span includes
   connection setup, upstream queueing, prompt prefill, decode, transport, SSE
   parsing, and post-stream validation. Two requests with identical model decode
   speed but different TTFT rendered wildly different "TPS", so the number could
   not be compared across networks, prompts, or cache states.
2. **Wrong home.** Token-budget UI answered "how fast?", performance never
   surfaced where latency lives (TTFT vs stream pace vs tail), and there was no
   per-attempt drill-down for timing at all.

Client-side measurement can be precise about *observation*, but it cannot see
inside the provider: queue time, prefill, and true decode pace are server-side
facts that only upstream telemetry can supply.

## Decision

Record one `RequestPerformance` sample per provider attempt inside its existing
`RequestUsageRecord`, settle it through the same idempotent terminal path, and
give it a first-class surface that owns no token-budget content:

- **Sampled stages** (monotonic microseconds relative to dispatch): stream-ready,
  observed TTFT (first output-bearing event), visible TTFT (first assistant-text
  event), stream span (first → last output event), tail (last output event →
  stream end), and end-to-end to validated response. First-event token count and
  output-event count ride along.
- **Honest absence.** Every stage is `Option`; legacy records deserialize to
  `performance: None`. Missing data renders `–` — never `0`, never an estimate
  dressed up as a measurement.
- **Stream rate excludes the first event's tokens** from the numerator
  (`(streamed − first-event tokens) / stream span`) because those tokens already
  existed when the stream clock began, and requires ≥2 events.
- **Three labeled scopes**, never one ambiguous number: **Observed/Visible TTFT**
  (perceived latency), **Stream rate** (client-observed pace excluding TTFT),
  **E2E output rate** (including TTFT). A fourth scope, **Server decode rate**,
  is reserved behind explicit provider-native fields (`provider_decode_us`,
  `provider_output_tokens`) that current providers do not send yet — reserved
  fields stay absent rather than being faked.
- **Surface split.** The hint bar gains an optional latest-turn rate segment
  between the input action and the model identity cluster; clicking it opens an
  independent **Performance** report (round list → per-turn/attempt drill-down)
  wired as its own retained panel with its own scroll/drill state. The Context
  Usage modal drops every throughput element and returns to tokens-only shape
  (its former TPS column reverts to the turn count).
- **Push + restore plumbing.** `RoundEvent::TurnPerformance(TurnPerformanceSnapshot)`
  is emitted per completed principal turn, mirrored into per-session chrome, and
  replayed from durable request records on attach/resume so the hint bar
  hydrates without waiting for a fresh round.

## Alternatives considered

- **Keep TPS in Context Usage and add columns.** Rejected: it preserves the
  conflated denominator, forces one modal to serve two questions, and repeats
  rows a user cannot act on while browsing token spend.
- **Subtract a measured RTT/ping from TTFT.** Rejected: a ping measures a
  different path and direction mix than the streaming POST; subtracting it does
  not recover queue+prefill and manufactures false precision.
- **Client-only "decode rate" estimated by dropping TTFT.** Rejected without
  server telemetry: `(last − first)` includes proxy buffering and network gaps;
  only the *observed* stream rate is defensible, so the strict decode metric is
  gated on provider-native fields instead.
- **Polling an on-demand query for the hint value.** Rejected: the round-driven
  push keeps the segment live during long rounds and costs one small Copy struct
  over the existing event channel.

## Consequences

- Positive: TTFT no longer contaminates pace; failures/retries keep their own
  attempts' timings; the strict decode metric has a typed landing place for
  future provider telemetry; Context Usage reads purely as token budget again.
- Negative: two panels read one ledger with different filters (success-only and
  master-only for rates); consumers must distinguish "no telemetry" (`None`,
  rendered `–`) from "measured zero".
- Neutral: legacy persisted records keep decoding unchanged; `generation_ms`
  stays populated for compatibility but new surfaces derive everything from the
  structured sample.

## References

- [Request lifecycle accounting](0055-session-scoped-request-lifecycle-accounting.md) —
  the per-attempt ledger this extends.
- [Durable cross-session usage statistics](0122-durable-cross-session-usage-statistics.md) —
  settlement fan-out the samples ride on.
- Explanation: [Token accounting](../explanation/agent-design/token-accounting.md).
