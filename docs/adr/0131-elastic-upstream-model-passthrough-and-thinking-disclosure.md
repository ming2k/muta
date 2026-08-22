# 0131. Upstream behavior is elastic input, not error: model passthrough and thinking disclosure

- **Status:** Accepted
- **Date:** 2026-08-22

## Context

neenee sits between a model catalogue (what we advertise the user) and upstream
model routes (what actually answers). Reverse-engineering Google's own CLI
(`agy` v1.1.17) established two facts that shaped this ADR:

1. **Upstreams serve model ids natively and distinct.** `agy`'s binary carries
   a static catalogue where `gemini-3.7-flash-{high,medium,low}` (enum
   1298–1300) and `gemini-3.6-flash-{…}` (1071–1073) are *separate* entries
   with separate ids, and 10k+ persisted turns show the enum recorded on the
   wire matching the user's selection — no remapping. Yet our
   `protocol/google/mod.rs` carried an uncommitted hard-coded
   `wire_model` remap (`3.7-* → 3.6-*`), quietly turning every 3.7 selection
   into a 3.6 request. That conflated three distinct things: what the catalog
   says, what the channel is configured for, and what some upstream happened
   to accept on the day the remap was written.

2. **Chain disclosure is the upstream's prerogative and varies per route.**
   The same `agy` binary shows a per-step thinking-disclosure rate of ~16%
   on 3.7 Flash versus ~46% on 3.1 Pro — same client, same flags. Upstreams
   also *reject* the disclosure request outright on some routes
   (`INVALID_ARGUMENT` naming `thinkingConfig` / `includeThoughts`), because
   the field is model-gated server-side. A middle layer that treats either
   behavior as an error (or, worse, silently narrows its catalogue to dodge
   it) is fragile in exactly the dimension it exists to absorb.

The general principle the user stated: **as a middle layer we must have the
robustness and extensibility to treat upstream behaviors — including hiding
the chain of thought — as expected inputs with local elasticity**, not as
failures or as reasons to lie about model identity.

## Decision

Three rules, all implemented in `crates/neenee-llm-client/src/protocol/google/`
and intended as the pattern for every protocol adapter:

1. **Model identity passes through verbatim.** The wire model id is exactly
   `endpoint.model` — what the channel was configured with. No adapter may
   remap, alias, or "stabilize" ids locally. When an upstream rejects an
   advertised id, the existing 404 clarifier makes it a loud, actionable
   error. Id correction, if ever needed, belongs in the catalogue/channel
   configuration layer where it is visible to the user — never in the
   transport.

2. **Chain-disclosure refusal is downgraded elastically, once per channel.**
   `response::rejects_thinking_config` recognizes the narrow error family
   (an invalid-argument signal *plus* a thinking-surface token, case- and
   spelling-tolerant for relays). On first observation the adapter latches a
   channel-scoped sticky flag (`GoogleProvider::thinking_rejected`), logs one
   warning, and retries the identical turn with the **entire**
   `thinkingConfig` omitted — both `includeThoughts` and the depth directive
   (`thinkingLevel`/`thinkingBudget`), since they ride the same rejected
   object. Later turns skip straight to the thinkingless form: at most one
   probe request per channel per process. The model keeps thinking
   server-side; what we lose is disclosure, which was never ours to command.

3. **An absent chain is a normal turn.** No `ReasoningDelta` ever arriving is
   not an error, a warning, or a missing feature: the transcript simply has no
   `MessageKind::Thinking` entry for that turn (the TUI already gates at
   message creation via `chain_disclosed()`, so absent chains leave no phantom
   layout entries). The only signal we surface is the one-time sticky-flag
   warning when the upstream *actively refused* the disclosure request —
   which is route-level information worth knowing.

## Alternatives considered

- **Keep the hard-coded id remap as "compatibility".** Rejected: it makes the
  catalog lie (the picker says 3.7, the wire says 3.6), misattributes
  billing/quota/capabilities, and rots invisibly the day the upstream starts
  serving the id natively — exactly what the `agy` evidence shows happened.

- **Mark models without chain disclosure as `ReasoningSummary` (hidden-chain)
  in the static catalogue.** Rejected for Google: disclosure is *route*-scoped
  (same model, different answer per account/region/灰度), not *model*-scoped.
  A static per-model label would be wrong on some channels and cannot react to
  the upstream's own rejection. The sticky runtime flag is the honest unit.

- **Retry without thinking on *every* request (no sticky flag).** Rejected:
  one refused probe per turn doubles latency on channels that will never
  disclose; the knowledge is channel-scoped and durable for the process, so
  pay for it once.

- **Surface the refusal as a user-visible error and let the user reconfigure.**
  Rejected: nothing about the turn failed. The answer streams fine; only the
  optional disclosure is unavailable. Failing the turn punishes the user for
  an upstream policy decision.

## Consequences

- **Positive:** one consistent posture — identity is honest, disclosure is
  best-effort, absence is normal. The adapter degrades without user action
  and tells them once (warn log) instead of never or always.
- **Positive:** the pattern extends: any protocol adapter that stamps an
  optional capability field an upstream may reject can adopt the same
  narrow-classifier + sticky-flag + full-surface-omission shape.
- **Negative:** the sticky flag is per-process, so a daemon serving long
  sessions re-probes after restart (one request). Accepted — probing more
  often than process lifetime risks fighting an upstream mid-rollout.
- **Negative:** users on a chain-withholding route see no thinking trace on
  Gemini even with thinking on. That is the upstream's choice; pretending
  otherwise (e.g. synthesizing a fake trace) was never on the table.
- **Neutral:** `stream_chat` (text-only streaming, used by tests/summaries)
  does not retry — it latches the flag and fails the call so the caller falls
  back to the event-stream path, which is the only path the agent drives.

## References

- `crates/neenee-llm-client/src/protocol/google/mod.rs` — passthrough,
  `note_thinking_rejected` / `thinking_was_rejected`, downgrade paths.
- `crates/neenee-llm-client/src/protocol/google/response.rs` —
  `rejects_thinking_config` classifier.
- [ADR-0046](0046-reasoning-is-opt-in-per-model.md) — reasoning opt-in; this
  ADR covers the orthogonal question of *disclosure availability*.
- `crates/neenee-contracts/src/thinking.rs` — `ThinkingSupport` /
  `chain_disclosed()` (message-creation gating for genuinely hidden-chain
  *models*, the static counterpart of this runtime mechanism).
