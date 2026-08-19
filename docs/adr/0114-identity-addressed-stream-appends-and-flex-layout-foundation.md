# 0114. Identity-addressed stream appends and the flex layout foundation

- **Status:** Accepted
- **Date:** 2026-08-21
- **Revises:** [ADR-0111](0111-transcript-entry-unification-and-concurrent-rendering.md)
  (delivers the entry-level concurrent rendering it specifies)

## Context

Two defects shared one root: transcript geometry was addressed positionally
("the last message") instead of by entry identity.

1. **Streaming fork.** ADR-0111 permits dispatching a command entry (Entry B)
   while a conversational turn (Entry A) is still streaming. But the
   reasoning-delta ingestion resolved its target with
   `msgs.last_mut().filter(is_thinking…)` — "is the last message a Thinking
   entry of this turn?". The moment `/autopilot` (or any local notice, or a
   tool step) was appended between two reasoning deltas, the filter failed and
   the next delta *created a second Thinking entry*. Users saw one reasoning
   trace split into two `+ Thinking · N chars` blocks. The same `last_mut()`
   pattern existed for assistant text deltas, `StreamEnd`, `StreamReasoningEnd`
   finalization, and envoy child folding (`EnvoyEvent::StreamDelta` /
   `StreamReasoningDelta`).
2. **Duplicated geometry.** Entry heights were computed by painting (or the
   height cache), then *re-derived* by a hand-rolled accumulation loop in
   `build_virtual_index`. Two independent implementations of the same
   arithmetic could drift; the engine's only layout solver was a
   ratatui-compatible 1-D `Min/Length/Percentage` splitter with no
   content-based sizing.

## Decision

### 1. Stream appends are identity-addressed

Every streaming append/finalize now resolves its target by scanning backward
for the entry matching `(round, turn)` **and kind**, never by position:

- `append_reasoning_delta(messages, round, turn, delta)` — Thinking entries;
- `append_stream_text_delta` — assistant text entries;
- `StreamEnd` / `StreamReasoningEnd` finalize by `(round, turn)` + streaming
  predicate;
- envoy child folding (`push_envoy_event`) resolves by kind + streaming
  predicate within the step's children.

An entry appended between deltas (command row, notice, tool step) therefore
cannot fork or steal a live stream. Cross-turn deltas can never graft onto an
older turn's entry: `(round, turn)` must match. Regression tests pin each
case (`reasoning_delta_appends_across_an_intervening_command_entry`,
`text_delta_appends_across_an_intervening_command_entry`, …).

### 2. The engine grows a flex layout subsystem (`neenee_tui_engine::flex`)

A pure-Rust flexbox-style solver — no CSS text, stylesheet, or parser; only
the layout algorithm (main/cross axis, grow/shrink/basis, justify/align,
gap), the same family as Yoga (React Native) and Taffy (Dioxus):

- `Flex::column()/row()` + `gap/justify/align` describes a single-level
  container; nesting is achieved by feeding a solved child rect back into an
  inner `Flex` (terminal layout is shallow; a retained tree would only add
  ownership burden).
- `FlexItem::fixed/auto/grow` with `min_main/max_main/cross` overrides;
  `Basis::Auto` consults a `measure(index, cross) -> main` callback — the
  intrinsic-size pass ("given this width, how many rows does this entry
  need?") the engine previously lacked.
- Integer-only solving: surplus grows by weight (floor + deterministic
  remainder to earlier items), deficits shrink weighted by
  `shrink × basis`, `min_main` outranks shrink (per spec), offsets are kept
  at `usize` precision (`SolvedFlex::main_offset/main_exact`) so scroll
  extents beyond u16 remain exact.

`Layout::split` is retained for ratatui API compatibility but is now a thin
mapping onto the flex solver (`Length/Percentage → fixed`, `Min → fixed +
grow`), making flex the engine's single layout algorithm.

### 3. Virtual-index geometry comes from the flex solver

`build_virtual_index` now plans chunks (message range + cached height) and
solves their line offsets through `Flex::column()` in a single pass over
`FlexItem::fixed(height)` items against an unbounded main axis. The virtual
index and the paint path share one geometry source by construction; the
duplicated accumulation loop is gone.

## Consequences

**Positive:**
- Command dispatch mid-stream no longer splits a Thinking trace; ADR-0111's
  "Entry B respects Entry A's growing height" finally holds end to end.
- One layout algorithm engine-wide; new UI can declare gap/grow/align intent
  instead of hand-rolling measure/place loops.
- Virtualization geometry can no longer drift from painted geometry.

**Neutral:**
- `Layout::split` semantics preserved exactly (old tests pass unmodified);
  new code should prefer `Flex`.
- Scans are backward `rfind` over the transcript — bounded in practice by
  per-turn entry counts, and reasoning deltas arrive high-frequency but the
  match is found within the current turn's tail.

## Alternatives considered

- **Keyed stream map (`stream_id → message_id`)** — rejected as the primary
  fix: it requires threading a new correlation id through provider events;
  `(round, turn)` already exists on every stamped component and is what the
  renderer groups by. A keyed map remains a future refinement if providers
  ever interleave two streams in one turn.
- **Cassowary/typed constraint solver crate** — rejected: integer flex
  solving is fully deterministic, allocation-light, and dependency-free.

## References

- [ADR-0111](0111-transcript-entry-unification-and-concurrent-rendering.md) —
  the entry-unification and concurrent-rendering model this delivers.
- CSS Flexbox spec (algorithm reference):
  <https://drafts.csswg.org/css-flexbox/>
