# 0157. Visible-TTFT stage removed from RequestPerformance

- **Status:** Accepted
- **Date:** 2026-08-28

## Context

ADR-0151 sampled six client-observed stages per provider attempt, one of
them **visible TTFT** — dispatch to the first user-visible assistant text
event, where "visible" classified `TextDelta` as visible and
`ReasoningDelta`/`ToolCallDelta` as not. It was positioned as the
"perceived latency" scope alongside observed TTFT, stream rate, and E2E
output rate.

Three findings from use broke that positioning:

1. **The classification is factually wrong for this product.** The TUI
   streams reasoning into the transcript live
   (`apps/tui/crates/mutx/src/event_loop.rs`, `ReasoningDelta` → `Thinking`
   message). During a long thinking phase the user *is* seeing text — the
   first user-visible characters arrive with the first reasoning delta,
   which is the raw TTFT anchor. The metric measured something the screen
   never showed.
2. **It is absent for most attempts.** In a coding-agent workflow the
   majority of turns are tool-only: no `TextDelta` ever arrives,
   `visible_ttft_us` stays `None`, and the turn detail renders `–`.
3. **It measures workload, not performance.** Where it does exist, its
   difference from TTFT is the streamed thinking duration — model behavior,
   already visible as reasoning token counts. As a performance signal it
   added noise dominated by a non-performance factor.

The aggregate tables compounded this: `observed_ttft_us` preferred
`visible_ttft_us.or(ttft_us)`, so the round list's "First" column and the
turn table's "TTFT" column silently mixed two metrics — text turns showed
"time to first body text" (inflated by thinking), tool-only turns fell back
to raw TTFT, and the overview median averaged across both meanings under a
single label.

## Decision

- Remove `visible_ttft_us` from `RequestPerformance`
  (`crates/muta-contracts/src/token_ledger.rs`) and the
  `first_visible_output_at` anchor plus the `visible` event classification
  from `RequestAccountingGuard` (`crates/muta-agent/src/agent/mod.rs`).
- Every TTFT surface (overview median, round "First" column, turn table,
  turn detail) shows one metric: client-observed TTFT, dispatch to the
   first output-bearing event of any kind (text, reasoning, or tool-call
  payload).
- The turn detail page drops its "First visible text" row; the remaining
  first-token rows are "TTFT (first output)", "Stream ready (headers)",
  and "Headers → first output".
- Wire compatibility: the field was `Option` with
  `serde(default, skip_serializing_if)`. New peers reading legacy records
  ignore the stale key; legacy peers reading new records deserialize the
  absent field to `None`. No protocol bump (ADR-0134); the change is
  wire-compatible in both directions.

## Alternatives considered

- **Keep the field, fix the aggregates to raw TTFT.** Rejected: leaves a
  serialized field and a collected anchor with no consumer, and the
  classification would still misrepresent "visible" for this TUI.
- **Redefine as "first visible signal"** (first `TextDelta` or
  `ToolCallDelta`, e.g. a tool row lighting up). Rejected: the tool row
  renders at dispatch, near the *end* of the request, so it would measure
  almost the full generation span — a third meaning for an already
  confused label. Revisit only if perceived-latency telemetry for
  tool-heavy rounds becomes a real requirement.
- **Keep reasoning invisible and also hide it in the TUI.** Rejected for
  irrelevance: the TUI's live thinking trace is a product feature, not a
  telemetry problem.

## Consequences

- Positive: one TTFT definition everywhere; the aggregate columns stop
  mixing meanings; the stream-event accounting loses a classification that
  existed only to feed a broken metric; one fewer wire field.
- Negative: "why did I wait so long before the model started answering?"
  no longer has a one-number answer in the report — it is answered by TTFT
  plus reasoning token counts instead.
- Neutral: legacy persisted records keep decoding unchanged (unknown field
  ignored); `wire.gen.ts` is regenerated without `visible_ttft_us`.

## References

- [Request performance telemetry](0151-request-performance-telemetry.md) —
  the design this narrows (partially superseded on the visible-TTFT stage
  and scope).
- [Wire-protocol negotiation](0134-wire-protocol-negotiation.md) — why no
  protocol bump accompanies the field removal.
