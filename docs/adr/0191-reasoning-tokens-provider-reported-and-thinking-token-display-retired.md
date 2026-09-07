# 0191. Reasoning tokens are provider-reported telemetry; the thinking-block token display is retired

- **Status:** Accepted
- **Date:** 2026-09-07

## Context

Three forces converge:

1. **The thinking summary shows a locally counted token number.** For
   `MessageKind::Thinking` messages the TUI renders `Thinking  148 tokens` /
   `Thought  1318 tokens (2.4s)` (ADR-0120 §5, implemented via
   `apps/tui/crates/mutx/src/model/document.rs::thinking_summary` and the
   ADR-0184 incremental `StreamingCounter`). That number is **not** the
   reasoning token count anyone cares about — it is a cl100k count of the
   *visible* chain text.

2. **Provider-reported reasoning tokens exist but are folded away.** The
   OpenAI Responses adapter reads `output_tokens_details.reasoning_tokens`
   and folds it into `completion_tokens`
   (`crates/muta-llm-client/src/protocol/openai/responses/response.rs`);
   chat-completions `completion_tokens_details.reasoning_tokens` is likewise
   folded by the upstream `usage` merge. [`TokenUsage`]
   (crates/muta-contracts/src/usage.rs) has no `reasoning_tokens` field, so
   the ledger, the usage report, and persistence cannot carry the
   authoritative number.

3. **Vocabulary split-brain.** The same concept is named three ways:
   stream events / protocol parsers / render module / config key say
   *reasoning* (`StreamReasoningDelta`, `[model_reasoning]`,
   `disclosure/renderers/reasoning.rs`), while the TUI document model and
   display copy say *thinking* (`MessageKind::Thinking`, "Thought through…").
   ADR-0046 named the opt-in knob `[model_reasoning]`; the provider-agnostic
   token-accounting name of record is `reasoning_tokens` (OpenAI's field).

Accuracy nuance: the local count is not wrong for chain-disclosing models
(`ThinkingSupport::chain_disclosed()` already gates message creation — GPT-5.x
summary-only models never produce a Thinking message), but the *visible chain*
and the *billed reasoning tokens* diverge for every provider that bills hidden
CoT while streaming only a summary, and cl100k approximates non-OpenAI
tokenizers. A number that is sometimes exact and sometimes off by an order of
magnitude, rendered identically, is not a number — it is noise.

## Decision

1. **`TokenUsage.reasoning_tokens` becomes a first-class field.** Provider-
   reported reasoning token counts are carried per turn as a diagnostic
   breakout, exactly like the cache counters: *already included* in
   `completion_tokens` when the upstream reports it that way, additive
   (serde `default`), `0` = unknown. The OpenAI Responses and
   chat-completions adapters parse `output_tokens_details.reasoning_tokens` /
   `completion_tokens_details.reasoning_tokens` into it. Google's Gemini
   reports its hidden reasoning budget as `thoughtsTokenCount` and is parsed
   into the same field. Anthropic sends no such counter and stays at `0`. The
   token ledger's `RequestUsageRecord` gains the mirrored field;
   `TokenSourceTotals` aggregates it.

2. **The thinking-block token display is retired — not replaced by a local
   fallback.** `thinking_summary` stops reporting any token count: no local
   count while streaming, none when settled. The summary line keeps the
   milestone/duration vocabulary (`Thinking through the database migration`,
   `Thought through 3 steps (2.4s)`). The `StreamingCounter` field on
   `MessageKind::Thinking` is deleted along with its per-delta feeding in
   `push_thinking_delta`/`finalize_thinking`. Provider `reasoning_tokens`
   will surface in the usage/performance surfaces where the authoritative
   number lives; it does not belong in a per-message header it cannot yet
   fill everywhere.

3. **One concept, one name: `reasoning`.** The TUI document model renames
   `MessageKind::Thinking` → `MessageKind::Reasoning`,
   `thinking_summary` → `reasoning_summary` (and siblings
   `push_thinking_delta` → `push_reasoning_delta`,
   `finalize_thinking` → `finalize_reasoning`, thinking constructors and
   helpers likewise). The contracts enum renames `ThinkingMode` →
   `ReasoningMode` and `ThinkingSupport` → `ReasoningSupport` (Rust names
   only; serde variants and wire values are unchanged). The config key
   `[model_reasoning]` and the user-facing display copy ("Thinking",
   "Thought through…") stay as they are — display copy is presentation, not
   vocabulary.

## Alternatives considered

- **Keep the local count for chain-disclosing models only.** Rejected: it
  makes the display conditional on a capability flag threaded into the
  document model, and the number still means "cost of the visible text",
  which users will read as "reasoning cost". Two surfaces showing two
  different numbers for one question is the drift we are deleting.
- **Replace the display with provider `reasoning_tokens` per message.**
  Rejected for now: usage arrives once per request, not per thinking block;
  multi-block turns cannot split it honestly. The ledger/usage report is the
  correct home; per-block attribution would be fabrication.
- **Rename to `thinking` everywhere instead.** Rejected: vendor-specific
  (Anthropic), collides with the accounting field name `reasoning_tokens`
  we are introducing, and loses to the majority of existing internal names.
- **Compat aliases for the renamed Rust types.** Rejected: type renames are
  not a wire break (serde values unchanged); `deprecated` aliases are the
  drift surface the no-compat policy exists to prevent.

## Consequences

**Positive.** One reasoning-token number, provider-authoritative, flows
through usage → ledger → persistence → usage report. The TUI stops showing a
number that silently ranged from exact to 10× off. Vocabulary is single.

**Negative.** No compatibility aliases are carried (the "erase over compat"
policy): `MessageKind::Thinking` and the thinking helpers are renamed without
deprecation shims; `Thinking`-keyed wire/TS type names change with regenerated
bindings. Old thinking summaries that displayed a token count are gone; no
persistence format changes (MessageKind is not serialized; serde values of the
renamed enums are unchanged).

**Neutral.** The local BPE tokenizer, `StreamingCounter`, and the
estimated-source ledger path (ADR-0117/0120/0044) are untouched — they remain
the fallback estimator for providers that report no usage. ADR-0120 §5's
"thinking header shows N tokens" sub-decision is superseded; the tokens-as-
unit policy stands.

## References

- [ADR-0044](0044-layered-token-accounting.md) — reported-vs-estimated
  accounting stands unchanged.
- [ADR-0117](0117-native-cl100k-bpe-tokenizer.md) — the estimator core stays.
- [ADR-0120](0120-tokens-first-class-unit.md) — supersedes §5's thinking-token
  display only.
- [ADR-0046](0046-reasoning-is-opt-in-per-model.md) — `[model_reasoning]`
  naming this ADR aligns with.
- [ADR-0184](0184-incremental-streaming-pipeline-and-single-parse-hot-path.md)
  — the incremental counter's O(1)-per-frame motivation is retired with the
  display it served.
