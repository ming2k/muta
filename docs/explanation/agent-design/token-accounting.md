# Token accounting

neenee measures context pressure in **tokens** because that is the unit every
model's context window is denominated in. A round's token count drives the three
context-projection layers — [pruning](context-pruning.md) →
[compaction](context-compaction.md) → overflow recovery — and the live meter in
the TUI's hint bar. Getting that number *approximately* right is what keeps the
agent from silently overflowing the window or, conversely, compacting far too
early.

This page is the single reference for **how neenee counts tokens**: the
two-source model (upstream-reported vs. locally-estimated), the char-class
estimator that backs the local path, the ledger that attributes every token to
its source, and the report modal that surfaces it all to the user. For the
layers that *consume* the count, see the compaction and pruning pages; for the
request flow that carries the usage object, see [Request flow](../request-flow.md).

## The two-source problem

A model provider knows exactly how many tokens a request consumed — it computes
the count while serving the request and returns it in a `usage` object. A
client that wants an accurate picture should use that number. But three things
get in the way of "just use the provider's number":

1. **Not every provider returns usage.** A local relay, a minimal OpenAI-compatible
   server, or a provider that strips the field simply has nothing to report.
2. **The number arrives *after* the request completes.** The harness needs a
   pressure estimate *during* a round (between tool turns) to decide whether to
   prune, and at that point the current request's usage is not back yet.
3. **The number is for one request, not the running total.** Each `usage` object
   describes one turn-trip's `prompt_tokens` + `completion_tokens`; the
   context pressure is roughly the *next* request's `prompt_tokens`, which is the
   size of the accumulated window.

neenee resolves this with a **layered** policy: prefer the upstream number when
it exists, fall back to a local estimator otherwise, and *attribute* every token
to its source so the user can see which counts are authoritative and which are
guesses.

## The priority chain

At the single booking point (`Agent::book_turn_usage`,
`crates/neenee-agent/src/agent.rs`), each round's usage is resolved through this
chain, in order:

```text
streamed Usage event  ──▶  take_last_usage()  ──▶  projected prompt +
(OpenAI include_usage,     (non-streaming chat       estimated completion
 Anthropic message_delta)   residual, Gemini)         (local char-class)
       │                          │                          │
       ▼                          ▼                          ▼
   reported: true            reported: true            reported: false
   (authoritative)           (authoritative)           (heuristic)
```

1. **Streamed `Usage` event.** When streaming, providers that support it emit a
   terminal usage chunk: OpenAI's `include_usage` terminal chunk, Anthropic's
   `message_delta.usage`. These arrive as `ProviderStreamEvent::Usage` and are
   captured into `streamed_usage` as the stream runs.
2. **`Provider::take_last_usage()`.** For non-streaming chat, or when the stream
   did not carry usage, the provider stashes the `usage` object internally and
   hands it out here. This is a *consume-once* drain: the value is cleared after
   reading so it can never be double-counted.
3. **Local request estimate.** The final fallback combines the exact pre-wire
   prompt projection with an estimate of the completed or observed output.
   This keeps estimated totals comparable with reported input-plus-output
   totals. The character estimator is described below.

Whichever source wins, the count settles the same request attempt and is tagged
as *reported* (the first two) or *estimated* (the third). That tag is what the
report modal shows.

### Why "streamed first, then drained"?

The two upstream paths are not mutually exclusive but are ordered for a reason:
a streaming round that emitted a `Usage` event already holds the authoritative
number in `streamed_usage`, so there is no need to also drain the provider's
stash (which may be empty or stale from a prior request). The `or_else` chain
makes "we already have the streamed number" short-circuit cleanly.

## The local char-class estimator

When no upstream usage is available, neenee estimates locally. The estimator
lives in `crates/neenee-core/src/pressure.rs` (`count_tokens`) and replaces the
old flat `bytes / 4` heuristic that the codebase carried for years.

### Why `bytes / 4` was wrong

The old estimator divided the UTF-8 byte length by four. That ratio is a
reasonable average for **English prose** — English words average about four
characters and BPE merges them into one token. But neenee's conversations are
seldom pure English prose: they are dense with **Chinese/CJK** and **source
code**, and for both `bytes / 4` breaks badly:

- **CJK is severely under-counted.** A Chinese ideograph is almost always *one
  token* in modern tokenizers (BPE vocabularies are trained on English-dominant
  corpora, so CJK glyphs rarely merge). But UTF-8 encodes one glyph as *three
  bytes*, so `bytes / 4` turns four Chinese characters (`人工智能`, ≈4 tokens)
  into `12 / 4 = 3` — and worse, a longer Chinese sentence can be under-counted
  by 3–4×, so the meter reads "30% full" when the window is actually near
  capacity.
- **Code is unevenly over-counted in spots and under-counted in others.** Code
  is full of brackets, operators, and indentation that BPE tends to split into
  single tokens, making it denser than `bytes / 4` predicts; but long
  identifiers (`getUserSettingsFromDatabase`) merge more than the heuristic
  assumes. The net error is large and unstable.

### The char-class model

The estimator classifies each Unicode scalar into a category and adds a
*fractional* per-character token weight, accumulating in fixed-point integer
math (scaled by 256) and rounding once at the end. It is a single O(n) pass
with no external vocabulary.

| Category | Weight (tokens/char) | Rationale |
|----------|:----:|-----------|
| ASCII letter / digit / whitespace | 0.25 | English baseline: BPE merges ~4 chars/token |
| CJK ideograph, kana, Hangul | 1.0 | Almost one token per glyph |
| CJK + fullwidth punctuation (。，、？！) | 1.0 | Low-frequency, usually its own token |
| Other non-ASCII letters (é, а, λ) | 0.5 | ~2 chars/token, denser than ASCII |
| Code punctuation `(){}[];` `=+-*/` | 1.0 | Dense, rarely merges with neighbors |
| Other ASCII punctuation (`. , " '`) | 0.5 | Merges more than operators, denser than words |

The CJK ranges covered are: CJK Unified Ideographs (+ Extension A/B–F),
Hiragana, Katakana (incl. halfwidth), Hangul Syllables, CJK Radicals, CJK
Compatibility Ideographs, and fullwidth ASCII letters/digits — everything a
modern tokenizer splits per-glyph.

The net effect: a 4-character Chinese phrase now estimates as **4 tokens** (not
1), a line of code estimates higher than prose of the same length, and plain
English stays close to the old `bytes / 4` number.

### Where the estimator is *not* used

The estimator measures the **content the provider will receive** (message
`content` + tool-call names/arguments, recursively including nested envoy
transcripts). It deliberately excludes:

- **`reasoning_content`** (extended thinking) — never sent to the provider, so
  it does not consume the next request's window.
- **Framing overhead** — per-message role tags, the chat template the serving
  runtime applies, system-prompt token counts from the provider's tokenizer.
  These are unknowable without the real tokenizer, so the estimator ignores
  them; this is the main reason the upstream number is always preferred when
  available.

## The token-source ledger

To make the reported-vs-estimated distinction observable, neenee keeps one
lifecycle record per concrete provider request:

```text
session
  └─ actor
      └─ round
          └─ turn
              └─ attempt → state + source + input/output/cache usage
```

The actor separates the principal from each envoy. A retry creates a new
attempt under the same round and turn because every request that reached the
provider may be billed. Attempts move from `in_flight` to `completed`,
`interrupted`, `failed`, or `abandoned`. An abandoned attempt is an in-flight
record recovered after a process crash.

Terminal events update the same keyed record. Reported usage can replace an
estimate, but an estimate cannot replace reported usage. This makes replay and
duplicate terminal events idempotent rather than double-counting them.

Records are persisted with the session. Opening or resuming a session restores
its own ledger; a fork begins with no copied request usage because the parent
requests must not be billed twice. Provider/model totals and turn-level rows
are derived from these records instead of serving as mutable primary state.

## The Context Usage modal

The hint bar's context meter — the `89.2k (8%)` indicator pinned to the
bottom-right — is now **clickable**. Clicking it opens a centered, read-only
**Context Usage** modal. The top section shows current AI-visible context. The
request-usage section groups the active session's attempts by provider and
model, and a detail page expands it into round, turn, and attempt rows:

```
┌─ Context Usage ───────────────────────────────────────────┐
│ Current AI-visible context                              │
│ Size                 12.5k / 200.0k (6%)                │
│ Source               local request projection           │
│ ──────────────────────────────────────────────────────────│
│ Request usage                                            │
│ openai · gpt-4o                 12.3k          100% real │
│ relay · local-model              2.4k          estimated │
│                                              Esc close     │
└────────────────────────────────────────────────────────────┘
```

The report answers two questions at a glance:

- **"What would the next request contain?"** — the current-context section is
  available before the first request and refreshes after committed history
  changes.
- **"How accurate is request accounting?"** — `% Real` describes the usage
  ledger, not the current-context projection. `100%` means every settled token
  for that model came from provider usage.
- **"Which of my providers actually report usage?"** — the row breakdown makes
  it obvious which models are measured and which are guessed, so a user
  debugging a premature-overflow or never-compacts issue knows whether to look
  at the estimator or at the provider.

The percentage colors signal accuracy at a glance: green when fully reported,
yellow when mixed, muted/red when all estimated.

## Current context vs. request usage

The two displayed quantities intentionally answer different questions:

| Quantity | Behavior | Used for |
|----------|----------|----------|
| **Current context** | Replaceable projection of the next provider input | Hint-bar meter, pruning, compaction |
| **Request usage** | Additive input/output usage for every network attempt | Provider/model totals, billing diagnostics |

Provider usage does not directly replace current context. Billed output can
include hidden reasoning or generated text discarded by an interrupt, neither
of which necessarily enters the next request. Conversely, a tool result or
context projection can change the next request without changing the previous
usage object. Current context is therefore recomputed from committed
AI-visible history, while provider usage remains authoritative only for the
request ledger.

## How each provider surfaces usage

The upstream path only works because each provider adapter now actually parses
the `usage` object it previously discarded:

| Provider | Non-streaming | Streaming |
|----------|---------------|-----------|
| **Anthropic** (`anthropic_compat.rs`) | top-level `usage.input_tokens` / `output_tokens` | `message_delta` event's cumulative `usage` → `Usage` event |
| **OpenAI-compat** (`openai_compat.rs`) | top-level `usage.{prompt,completion,total}_tokens` | requests `stream_options.include_usage`; terminal chunk's `usage` → `Usage` event |
| **Gemini** (`neenee-ai-sdk-google`) | `usageMetadata.{prompt,candidates,total}TokenCount` | `usageMetadata` in stream payloads → `Usage` event |

Each adapter implements `Provider::usage_supported() -> true` and stashes the
parsed `TokenUsage` in an internal `Mutex`, drained by `take_last_usage()`. The
The active provider and model are captured before dispatch, so a later
mid-session provider switch cannot reattribute an in-flight request.

Providers that never report usage (test doubles, a relay that strips the field)
keep the trait's default: `usage_supported() -> false`, `take_last_usage() ->
None`. The booking chain falls straight through to the estimator, and the ledger
records those attempts as estimated — exactly as the report shows.

## The `CHARS_PER_TOKEN` constant, and why it still exists

`pressure::CHARS_PER_TOKEN = 4` remains in the codebase, which can look
contradictory after reading the above. It is retained because the **reverse**
direction — converting a *token* budget into a *character* budget — still uses
it, deliberately:

- `summary_char_budget` turns a target token count into a max character budget
  for a compaction summary.
- `prune_protect_chars` turns a protect-token budget into characters to shield.

In that direction (tokens → characters) the flat ratio is a **safe over-estimate
of characters**: it gives the summarizer more room and the protector a wider
shield, which are both conservative. The char-class estimator is only better
than `bytes / 4` in the forward direction (text → tokens); for budget sizing the
flat constant is fine and changing it would shift every compaction threshold. So
the constant lives on as a one-way conversion factor, clearly distinct from the
estimator.
