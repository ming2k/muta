# 0120. Tokens are the first-class unit: retire char-denominated budgets and displays

- **Status:** Accepted
- **Date:** 2026-08-20

## Context

ADR-0117 made token counting exact and cheap (native `cl100k_base` BPE).
Before that, a character-class heuristic stood in for a tokenizer, and the
codebase grew a layer of **character-denominated budgets, markers, and
displays** around it — each a proxy for the number everyone actually cares
about: how much of the model's context window a piece of text consumes.

The audit that motivated this ADR found the char layer had also drifted into
incoherence:

- `RoundEvent::Compacted { before_chars, after_chars }` is populated from
  `estimate_bytes()` — the **values are bytes**, the field names say chars,
  the TUI renders them as `bytes`, and the web panel as `chars`. Four
  disagreements on one number.
- `cleared_placeholder` and `truncate_middle` report `content.len()` — bytes —
  inside strings labeled `chars`, in **model-visible** content.
- `prune_tool_results(protect_recent_chars, min_reclaim_chars)` compares byte
  sums against thresholds, while every gate that decides *whether* to prune is
  token-denominated. The two units meet only through `CHARS_PER_TOKEN = 4`.
- The config key `compaction_prune_protect_tokens` is **already tokens**, but
  two call sites multiply it by `CHARS_PER_TOKEN` to feed the char-space
  pruner — a conversion that exists solely for the pruner's byte accumulator.
- `summary_char_budget = target_tokens × 4` converts a token target into a
  char budget, then `truncate_summary_to_token_budget` binary-searches the
  summary **back into tokens**. The char round-trip is pure loss.
- The TUI shows `Thinking · 140 chars` — a count of Unicode scalars — for
  reasoning blocks whose only cost that matters is context: tokens.

With an exact tokenizer in the dependency-free contracts crate, every one of
these proxies can be the real number at negligible cost.

## Decision

Make **tokens the single unit of account** across the pressure → prune →
compact pipeline and every transcript display of text volume. Concretely:

1. **Pruning goes token-native** (`neenee-contracts/src/pressure.rs`):
   `prune_tool_results(protect_recent_tokens, min_reclaim_tokens)`;
   `PruneOutcome::reclaimed_tokens`; the recency-protection accumulator and
   reclaim deltas use `estimate_message_tokens`. Tier thresholds
   (`TRUNCATE_MIN_TOKENS = 512`, keep-each-side `128 tokens`) and the min
   reclaim floor (`2 000 tokens`) are token-denominated. The `× CHARS_PER_TOKEN`
   conversions at the call sites are deleted — the config value passes through
   unchanged.
2. **Model-visible markers report tokens**: the cleared placeholder reads
   `(42 lines, 350 tokens)` and the elision marker `[... 1200 tokens elided
   to relieve context ...]`. Recognizers (`is_cleared` / `is_truncated`)
   match on stable substrings so sessions pruned by older builds still
   escalate correctly.
3. **Token-bounded truncation** everywhere a text cut previously used a char
   budget: `tokenizer::truncate_to_tokens` (exact rank-prefix cut) replaces
   char/byte caps in `truncate_middle`, the excerpt summary, the summarizer
   transcript caps (`SUMMARY_ENVOY_CAP`, `SUMMARY_TOOL_OUTPUT_CAP`,
   `EXCERPT_CAP`), the `[Previous summary]` carry-forward, and nested
   sub-envoy shares. `summary_char_budget` and the binary-search
   `truncate_summary_to_token_budget` are deleted.
4. **The Compacted event tells the truth**:
   `RoundEvent::Compacted { before_tokens, after_tokens }` (and the persisted
   `ContextProjectionCheckpoint`) carry `estimate_tokens` values, with serde
   aliases on the old `*_chars` names so historical session records replay.
   TUI and web render `N → M tokens`.
5. **Transcript displays show tokens**: the thinking header becomes
   `Thinking · N tokens`; tool-output truncation notices
   (`[Output truncated: N tokens total]`), fetched-content framing, search
   truncation markers, and file-write confirmations report the context-relevant
   number. Editor surfaces that describe *typing* (clipboard paste toasts,
   title display width, image byte sizes) intentionally stay in their natural
   units — they are not context-volume claims.
6. **Historical baggage is deleted, not deprecated**: the char-class estimator
   (`count_tokens_heuristic` and its weight tables), `CHARS_PER_TOKEN`, and
   the char round-trips have no callers and are removed. The BPE tokenizer is
   total (every byte is a token, encoding never fails), so a "fallback
   estimator" has no failure mode to fall back from.

## Alternatives considered

- **Keep chars as the internal unit, convert at the display edge.** Rejected:
  the mismatch is not cosmetic. The pruner's byte accumulator and the gates'
  token thresholds already disagree in direction (bytes under-count CJK by
  3–4×, the exact regression ADR-0044 fixed for estimation). Keeping two units
  guarantees the drift returns.
- **Add `*_tokens` fields alongside `*_chars`.** Rejected: doubles the wire
  surface and leaves the lie in place for every consumer that keeps reading
  the old fields. A serde alias gives old records a defined meaning; new
  records carry one honest number.
- **Keep the char-class estimator as a documented fallback.** Rejected: total
  tokenizer, no failure path, no caller. Dead code with a doc comment is still
  dead code (ADR-0044's rationale presumed a costly vocabulary that
  ADR-0117 eliminated).

## Consequences

**Positive.** One unit end to end: config, gates, budgets, markers, events,
and displays all mean the same thing. CJK-heavy sessions prune and compact at
the right moments (the byte accumulator under-protected them). The model sees
token counts in its own currency when deciding whether to re-fetch a cleared
result. The Compacted notice stops being wrong in four different ways.

**Negative.** No compatibility aliases are carried (the "erase over
compat" policy): records written before a rename — event tags
(`compaction_committed`, `repeat_jobs_set`, `turn_counter_set`), snapshot
keys (`messages`, `archived_messages`, `last_relief`, `repeat_jobs`,
`turn_counter`, `compaction_preserve_turns`), checkpoint fields
(`before_chars`/`after_chars`), the `PRUNED_TOOL_PLACEHOLDER` string, and the
`chars elided` marker — do not parse; the event loader skips such lines with
a warn and an unparseable snapshot starts a fresh session (loudly warned).
Old sessions effectively resume from their newest parseable state or reset;
that data loss is accepted in preference to a compat surface. Wire consumers
must move to the current field names (generated types are regenerated
in-repo; external consumers ride the version bump). Prune passes tokenize
the messages they walk (~10 MB/s measured, ~50 ms on a 200k-token history,
at prune cadence).

**Neutral.** `estimate_bytes` survives solely as a `/debug preview` diagnostic
(wire-size question, byte is the honest unit). Editor-typing surfaces stay in
chars by design (§5).

## References

- [ADR-0117](0117-native-cl100k-bpe-tokenizer.md) — the native BPE tokenizer
  this builds on.
- [ADR-0044](0044-layered-token-accounting.md) — whose "keep
  `CHARS_PER_TOKEN` as a one-way conversion" decision this supersedes; the
  layered reported-vs-estimated policy itself stands unchanged.
