# 0184. Incremental Streaming Pipeline and Single-Parse Hot Path

- **Status:** Accepted
- **Date:** 2026-09-06

## Context

Long-running sessions exhibit progressive streaming degradation: token
throughput visibly slows as a turn accumulates content, independent of
provider latency. Investigation located the root cause in the rendering and
protocol layers: the pipeline treats the live (streaming) message as an
immutable document that happens to change, and re-derives everything from
scratch on every change. Per-frame cost grows with accumulated content,
yielding O(n²) total work per stream. Three defect sites were identified:

### 1. Per-frame full BPE tokenization of the reasoning chain

`Message::thinking_summary()` (`apps/tui/crates/mutx/src/model/document.rs`)
calls `muta_contracts::tokenizer::count_tokens(content)` — a full
`cl100k_base` BPE pass over the entire accumulated thinking content — plus two
full-text scans (`extract_active_milestone`, `count_milestones`), on every
invocation. Because streaming Thinking messages are never `skippable`
(`apps/tui/crates/mutx/src/layout/mod.rs`), the height cache is bypassed and
the full render path executes every frame (~20 fps). A 10k-token reasoning
chain at 20 fps costs hundreds of thousands of display-only BPE tokens per
second, saturating the render thread and starving delta consumption.

### 2. Per-frame full re-parse and re-wrap of the live message

`Document::push_stream()` (`document.rs`) appends the delta and calls
`reparse()`, which runs `parse_blocks` over the *entire* accumulated raw text
(including inline scans) on every coalesced delta batch. `bump_rev()`
invalidates the `(id, rev)`-keyed `HeightCache` wholesale, forcing
`wrap_text` (`text_layout.rs`) to re-wrap the whole message every frame in the
body draw path. The rev-based cache only helps stable messages; for the live
message it is a no-op.

### 3. Per-chunk constant-factor overhead on the inference hot path

- Every SSE payload is parsed twice: OpenAI chat-completions performs a
  `serde_json::from_str::<Value>` validity pre-check before
  `stream_events` parses again; Anthropic's `SignatureStash::capture`
  fully deserializes every payload to find `signature_delta`, although ~99% of
  events are `text_delta`.
- `StreamLoopDetector::push_and_check`
  (`crates/muta-agent/src/stream_loop_detector.rs`) allocates three
  `Vec<char>` buffers and re-scans the whole 1024-char window on every
  delta, despite the underlying checks (KMP periodicity, digit density,
  monotonic streak) being online algorithms that can be maintained
  incrementally.

These constant factors do not grow over time, but they sit on the hottest path
and compound with (1) and (2) when the CPU saturates.

## Decision

Adopt the incremental-computation principle end-to-end: **every byte is parsed
once, wrapped once, measured once.** Replace "re-derive from zero on change"
with "state advances incrementally with input" at three layers.

### 1. Streaming token accounting on the Thinking message

Maintain a `StreamingCounter` (provided by
`muta_contracts::tokenizer`, already used correctly by the harness side) as a
field on `MessageKind::Thinking`. Feed each reasoning delta into the counter
at append time; `thinking_summary()` reads the accumulated count instead of
re-tokenizing. The two full-text milestone scans are likewise reduced to
incremental maintenance over the tail region. The displayed token count and
milestone semantics are unchanged.

### 2. Frozen-prefix incremental document and block-level wrap cache

Restructure the live-message document pipeline around a frozen prefix and a
mutable tail:

- **Resumable incremental parser.** The document stores `Vec<Block>` plus
  parser tail state (open fence, list continuation) and a stable boundary into
  the raw text. Markdown blocks are line-oriented: once terminated (blank line
  after a paragraph, closed fence), a block can never be modified by future
  input. `push_stream` resumes parsing from the tail state and re-parses only
  the open tail block; when the tail block terminates it is frozen and the
  boundary advances. Parse cost becomes O(delta) amortized. Rare paths
  (final sanitize at StreamEnd, message edits) fall back to full reparse.
- **Content-addressed per-block wrap cache.** Replace the wholesale
  `(id, rev)` height cache with a per-block cache keyed by (block content,
  width) → `Vec<WrappedLine>` + height. Frozen blocks (guaranteed immutable by
  the parser contract) are wrapped exactly once per width, for the lifetime of
  the document. The live tail block re-wraps only itself; because greedy
  wrapping is an online algorithm, appending text can only affect the final
  visual line, so even tail re-wrap resumes from retained line state. The
  `skippable` special case for streaming Thinking messages is deleted: with
  block-granularity caching, the frozen portion of a live message hits cache
  naturally.
- **Composition-only rendering.** `draw_message_body` composes from cached
  wrapped-line vectors and materializes only rows inside the existing virtual
  window; no re-styling of stable content.

### 3. Single-parse typed event stream and zero-allocation loop detector

- **One parse, typed consumers.** The LLM-client protocol layer parses each
  SSE payload exactly once into a typed stream event enum
  (`TextDelta`, `ReasoningDelta`, `SignatureDelta`, `Usage`, `Error`, …) with
  borrowed string fields. All downstream consumers — `stream_events`,
  `SignatureStash`, echo filter, loop detector — consume the typed event. The
  OpenAI validity pre-check is deleted; deserialization errors propagate from
  the single parse site.
- **Streaming loop detector.** `StreamLoopDetector` becomes a stateful online
  automaton over a fixed ring buffer: KMP automaton state carried across
  deltas instead of rebuilt per delta, digit density maintained by incremental
  push/pop counters, monotonic streak carried as state. Per-delta cost is
  O(delta) with zero allocation; verdict semantics are preserved.

### Regression guardrails

Add criterion benchmarks asserting that per-frame render cost is flat (O(delta),
not O(n)) across a 100k-token synthetic stream, and that per-delta harness cost
is allocation-free. These guard the entire class of regression, not just the
three defect sites.

## Alternatives considered

- **Move rendering off-thread (rayon/spawn):** hides O(n²) behind another core;
  the quadratic work still burns through on long streams and adds
  synchronization complexity. Rejected as symptom treatment.
- **Throttle live-message re-processing (every N frames / X bytes):** trades
  user-visible latency and liveness for performance. Rejected as a compromise.
- **Cap displayed content length / collapse long reasoning:** changes product
  behavior to dodge an engineering defect. Rejected.
- **simd-json / borrowed deserialization as the headline fix:** a useful
  garnish, but structural single-parse-one-consume is the actual fix;
  simd-json remains an optional follow-up on top of the typed event enum.
- **Append-only Text block until StreamEnd (the historical design):** defers
  all Markdown structure to stream end, causing the whole response to jump.
  The frozen-prefix design retains per-frame structural consistency while
  restoring incrementality.

## Consequences

### Positive

- Streaming throughput is independent of accumulated content: frame cost is
  O(delta) per layer, eliminating the O(n²) degradation entirely.
- Height-cache bypass special cases are removed; frozen blocks are measured
  once per width for the document's lifetime, also benefiting scroll-back
  re-layout after resizes.
- Per-token latency baseline drops (single JSON parse, zero-alloc detector).
- The architecture matches the stated intent of the versioned event pipeline:
  no long-session sluggishness, by construction.

### Negative & Mitigations

- **Parser state complexity:** the resumable parser must handle fence/list
  corner cases. *Mitigation:* full reparse remains as the fallback for any
  state the tail parser cannot resume (and for StreamEnd sanitize); existing
  markdown snapshot tests run against both paths.
- **Cache invalidation subtleties:** per-block content-addressed caching
  introduces new invalidation rules. *Mitigation:* block immutability is a
  type-level contract of the frozen prefix; the cache is keyed by content, so
  stale entries are unreachable by construction.
- **Scope:** layer 2 is an architecture-level change to the mutx document and
  layout pipeline. It is staged into independently verifiable phases:
  (a) StreamingCounter accounting, (b) incremental parser, (c) block-level
  wrap cache and layout integration, (d) typed event stream + detector rewrite
  (independent of a–c), (e) benchmark guardrails.

## References

- [ADR-0114: Identity-Addressed Stream Appends and Flex Layout Foundation](0114-identity-addressed-stream-appends-and-flex-layout-foundation.md)
- [ADR-0117: Native cl100k BPE Tokenizer](0117-native-cl100k-bpe-tokenizer.md) — provides `StreamingCounter`
- [ADR-0038: In-House Grid Diff Rendering Engine](0038-in-house-grid-diff-rendering-engine.md)
- [ADR-0151: Request Performance Telemetry](0151-request-performance-telemetry.md)
