# 0117. Native cl100k_base BPE tokenizer for token prediction

- **Status:** Accepted
- **Date:** 2026-08-19

## Context

Token prediction — the numbers behind the context meter, the pruning and
compaction triggers, `/context`, and the tool-schema overhead estimate — runs
through `count_tokens` in `crates/neenee-contracts/src/pressure.rs`: a
character-class heuristic that weights each Unicode scalar (ASCII word ≈ 0.25
tokens, CJK glyph ≈ 1.0, code punctuation ≈ 1.0) and sums. ADR-0044 rejected
bundling a real tokenizer mainly on vocabulary size, and the heuristic was
accepted as "good enough" *because* the authoritative path is provider-reported
usage.

Two things changed since ADR-0044:

1. **The heuristic is measurably wrong where neenee is used most.** On this
   repository's own sources the estimator disagrees with a real
   `cl100k_base` tokenizer by −29% to −46%: Rust source 8 522 estimated vs
   11 207 real (−24%), mixed Chinese/code sentences 36 vs 54 (−33%), CJK +
   fullwidth punctuation 25 vs 54 (−54%). Because compaction fires at a
   fraction of the *model window* (85%) while the meter runs low, a session
   can approach provider-side truncation while the UI reads green. Code
   punctuation at a flat 1.0 token over-charges some operators and
   under-charges merged pairs; there is no vocabulary, so it cannot know that
   ` fn` is one token but `fn` + `(` is two.
2. **The authoritative path never feeds the meter.** The envoy audit that
   preceded this ADR confirmed `ContextTokenSource::Api` is defined but never
   constructed; every `ContextTokens` emission uses `::Projection`. The
   projection is the meter.

The user direction for this change was explicit: replace the crude estimator
with a real BPE implementation following OpenAI's `tiktoken`, implemented
natively in Rust.

## Decision

Implement a native BPE tokenizer in `neenee-contracts`
(`src/tokenizer.rs`), following tiktoken's algorithm and data format:

1. **Vocabulary.** Ship OpenAI's `cl100k_base` vocabulary as a compact binary
   blob (`vendor/cl100k_base.packed`, 1 044 878 B) embedded with
   `include_bytes!`. The blob stores the 100 256 ranks in tiktoken's own
   ordering (line number = rank), so no per-entry rank field is needed:
   `u32` version, `u32` token count, `u64` blob length, then a `u32` byte
   length per token, then all token bytes concatenated in rank order.
2. **Algorithm.** Byte-level BPE exactly as tiktoken:
   - the GPT-2-family pretokenizer regex is implemented as a **native
     hand-rolled scanner** (no `regex` dependency — `neenee-contracts` stays
     dependency-neutral): `'(?:[sdmt]|ll|ve|re)| ?\p{L}+| ?\p{N}+| ?[^\s\p{L}\p{N}]+|\s+(?!\S)|\s+`;
   - each pretoken's bytes are merged greedily by lowest rank, using
     tiktoken's `byte_pair_merge` (smallest-ranked adjacent pair wins,
     ties by position);
   - lookups are exact matches against the packed table (see below); bytes
     with no merge stay as their single-byte tokens — every single byte is in
     the vocabulary, so encoding never fails and is total over arbitrary
     input.
3. **Table.** The blob is parsed lazily (first use) into a flat
   `Box<[u8]>` + prefix-length slice index; an auxiliary `HashMap<&[u8], u32>`
   covers only tokens ≥ 2 bytes that are valid whole-UTF-8 scalars
   (the multibyte starts that BPE actually needs); single-byte tokens are
   looked up in a `[u16; 256]` array (every byte value is a token). Merge
   lookups during `byte_pair_merge` consult the full 100 256-entry hash map.
4. **Per-message framing.** [`estimate_message_tokens`] keeps charging a
   small per-message overhead (3 tokens for the first system message, 4
   per message, 2 per tool call), following tiktoken's `chatml_example`
   measurement of how OpenAI chat framing tokenizes. Content, tool names and
   JSON arguments go through the tokenizer.
5. **Estimator retention.** `count_tokens` (char-class) is kept for the
   hot loop's *projection fallback* path only where a caller explicitly wants
   the cheap path; all pressure/compaction/`/context` call sites move to the
   tokenizer. The old estimator's tests stay (it remains a documented
   fallback), and its doc comment is updated to point at the tokenizer as the
   primary predictor.

## Alternatives considered

- **Keep the char-class estimator (ADR-0044 status quo).** Rejected: the
  measurement above shows −24% to −54% error on the repository's own content
  mix, and the meter/trigger path has no authoritative source feeding it.
- **Depend on the `tiktoken-rs` crate.** Rejected: it pulls `fancy-regex`
  (backtracking, compile time, and it transitively enables heavy features)
  and embeds vocabularies via build-time download or a separate file — both
  break the workspace's dependency discipline (`neenee-contracts` currently
  has no heavyweight deps) and offline reproducibility.
- **Vendor the Rust `tiktoken` C++ FFI.** Rejected: a C++ toolchain
  requirement for a Rust-native codebase, plus the same vocabulary-size
  objection handled here by compact packing (1.04 MB total, ≈35% smaller than
  the original 1.68 MB `.tiktoken` file).
- **Ship multiple vocabularies (o200k_base, per-model).** Deferred: the
  per-family error between cl100k and o200k on the same text is small
  relative to the current heuristic's error; adding o200k doubles the
  embedded payload. The `Tokenizer` API takes an encoding parameter so a
  second table can be added without API breakage.
- **Feed provider usage into the projection.** Kept as a separate problem:
  ADR-0044's layered accounting is unchanged; this ADR only replaces the
  estimator, not the booking policy.

## Consequences

**Positive.** Token prediction becomes exact for cl100k-family models
(GPT-4/4o/4.1/5 via OpenAI-compatible relays, most Chinese relay targets) and
a close approximation elsewhere, replacing −24…−54% error with single-digit
percent error. The meter, pruning, and compaction triggers all move together.
First-use parse of the 1 MB blob is a few ms, then the token map is shared
process-wide via `OnceLock`.

**Negative.** +1.04 MB embedded payload in every binary. Encoding is O(n)
with a hash lookup per merge candidate — roughly 5–20× slower than the
char-class estimator (measured ≈25 MB/s on this corpus vs ≈500 MB/s), which
is acceptable for meter refreshes and compaction gates on realistic session
sizes (a 200k-token history is ≈800 KB of text ≈ tens of ms). Estimating a
whole 1M-token transcript on every keystroke would be noticeable; the UI
already throttles context snapshots.

**Neutral.** `neenee-contracts` gains its first embedded data asset and a
`vendor/` directory convention for compacted upstream data (documented in
`vendor/README.md` with the generator). The old estimator remains as a
documented fallback and for tests that want determinism without the table.

## References

- tiktoken: <https://github.com/openai/tiktoken> (Rust core, MIT)
- tiktoken data format: <https://github.com/openai/tiktoken/blob/main/README.md>
- cl100k_base vocabulary:
  <https://openaipublic.blob.core.windows.net/encodings/cl100k_base.tiktoken>
- Supersedes the "bundle a real tokenizer — Rejected" alternative in
  [ADR-0044](0044-layered-token-accounting.md); ADR-0044's layered
  accounting policy itself remains in force.
