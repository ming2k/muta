# Prompt caching and cost control

> Companion to [ADR-0067](../../adr/0067-modular-prompt-cache-control.md)
> (the *decision*) and [Token accounting](token-accounting.md) (how tokens are
> *counted*). This page is about how caching *saves* them — and the one rule
> that keeps the savings honest.

Prompt caching is the dominant cost lever for a multi-round agent session. A cached
prefix is billed at roughly **0.1× input** (Anthropic) or folded into a
discount (OpenAI, Gemini, Moonshot). On a long coding session the stable
prefix — system prompt, tool schemas, recent rounds — is the bulk of every
request, so whether that bulk is read from cache or re-billed at full price is
the difference between a cheap session and an expensive one.

The catch: every provider exposes caching through a *different* surface, and a
missed cache field is a **missed discount** — a direct billing error. This page
documents the three strategies, where each provider hides its numbers, and the
single rule that prevents drift.

## The three strategies

A model family's caching strategy is classified once, in pure domain code, by
`CachePolicy::for_family(family)` in `neenee-core::cache`:

| `CachePolicy` | Who | What the client does | What the response reports |
|---------------|-----|----------------------|---------------------------|
| `Breakpoints` | Anthropic | Stamps explicit `cache_control: {"type":"ephemeral"}` breakpoints (≤4) across tools → system → messages | Separate `cache_creation_input_tokens` (write, premium) and `cache_read_input_tokens` (read, ~0.1×) |
| `SessionKey` | Moonshot / Kimi | Sends a session-scoped `prompt_cache_key` so the server cache namespaces per conversation | A single read count (`cached_tokens`) |
| `Automatic` | OpenAI, Gemini, and everything else | Nothing — the server auto-caches | A single read count, wherever the provider hides it |

The classifier is the only place that knows "kimi means session key, claude
means breakpoints". Request builders and the token ledger branch on its
predicates (`stamps_breakpoints`, `injects_session_key`) and never hard-code
provider names.

## How each provider surfaces its cache count

The read side is the billing-critical one. Each OpenAI-compatible relay / API
puts the cached count in a slightly different JSON path:

| Provider / API | JSON path | Notes |
|----------------|-----------|-------|
| Anthropic | `cache_read_input_tokens` (+ `cache_creation_input_tokens`) | The only provider with a separate *write* counter; `input_tokens` is the uncached suffix and must be folded in. |
| OpenAI chat-completions | `prompt_tokens_details.cached_tokens` | Auto-cache; no write counter. |
| OpenAI Responses API | `input_tokens_details.cached_tokens` | **Different key** from chat-completions — easy to miss. |
| Moonshot / Kimi | `cached_tokens` (top-level, proprietary) | Surfaces under the OpenAI-compat shape. |
| Google Gemini | `cachedContentTokenCount` (under `usageMetadata`) | Gemini's "context caching" surface. |

All of these collapse to one field on the harness's shared per-turn type:
`TokenUsage::cache_read_input_tokens` (`cache_creation_input_tokens` stays `0`
for everyone except Anthropic, which is the only one with an explicit write
tier). From there the value flows into the
[token-source ledger](token-accounting.md#the-token-source-ledger) and the
Context Usage modal's "Cache read / write" + hit-rate display.

## The one rule (and why it exists)

> **Every per-protocol `usage()` parser in the SDK layer MUST route its
> cache-read count through `neenee_core::cache::read_cached_tokens()`, never
> read the field inline.**

This is not a style preference. It is the single structural lever against
**billing drift**, and it exists because the alternative already cost money
once.

### The drift that almost shipped

When multi-provider cache accounting was first added, the three response
parsers were inconsistent:

```
openai/response.rs         → called read_cached_tokens()            ✓
responses/response.rs      → inlined input_tokens_details inline    ✗
google/response.rs         → inlined cachedContentTokenCount        ✗
read_cached_tokens() itself → did not know about input_tokens_details
```

The Responses API's `input_tokens_details.cached_tokens` is a *different key*
from chat-completions' `prompt_tokens_details.cached_tokens`. Because the
Responses parser forked its own read and the helper didn't list that key, the
discount was being read — but only by luck of the inline path, and any future
policy change in the helper (coefficient folding, zero-count-as-miss auditing,
a new relay field) would have silently skipped the Responses path. A missed
field is a missed discount, which is a direct cost error.

The fix was to (a) teach the helper the missing key and (b) route all three
parsers through it — plus a regression test
(`reads_openai_responses_input_tokens_details`) that fails if the key is ever
dropped from the helper again.

### Why centralizing prevents the whole class

Cache accounting is policy, not parsing. If the policy ever needs to change —
say, to fold a provider-specific discount coefficient, or to treat a zero
count as a billable cache-miss for auditing — it must change in **one place**.
Letting each protocol read its own field means a change touches N call sites,
and forgetting one is both likely (they're in different files/crates) and
invisible (no test catches "this protocol quietly stopped applying the new
policy"). Routing through `read_cached_tokens()` collapses that to a single
edit point with a single test surface.

## Where the strategies act (request side)

The write/control side is per-strategy and lives in the provider construction
layer (`build_provider_for_channel` in `neenee-providers`), which already holds
the concrete provider type and the model family:

- **`Breakpoints`** — the Anthropic request builder stamps up to four
  `cache_control: {"type":"ephemeral"}` breakpoints (last tool → last system
  block → two newest messages). This was already correct before ADR-0067 and is
  untouched.
- **`SessionKey`** — `OpenAiChatCompletionsProvider` carries an optional
  `prompt_cache_key`; the construction layer sets it to the **session id** when
  the family resolves to `SessionKey`. The session id is read from
  `Agent::thread_id()` inside the model-switch path, so no new parameter
  ripples through the dispatch arms. The key namespaces the server-side cache
  per conversation so repeated prefixes hit.
- **`Automatic`** — nothing is stamped; the server decides.

## Extending: adding a provider or a new field

**A new OpenAI-compatible relay** (DeepSeek, Qwen, …) almost always reuses the
existing `openai/response.rs` parser, so its `cached_tokens` is covered with
**zero code**. Just confirm the relay reports the field under one of the known
paths.

**A genuinely new JSON path** (some provider invents `cache_stats.hit`) is a
one-line addition to `read_cached_tokens()`'s key list — and every protocol
benefits immediately. Add a matching case to the helper's test module.

**A new model family with a different strategy** is a one-line addition to
`CachePolicy::for_family()`'s match. If the strategy is genuinely new (not
breakpoints / session-key / automatic), that's an ADR, not a quick edit — the
three-strategy partition is load-bearing.

## What is deliberately *not* here

- **No monetary cost layer.** neenee tracks tokens, not dollars. There is no
  per-model price field and no `$` anywhere in the ledger; the cache savings
  are realized as fewer tokens, surfaced as a hit rate. (Adding pricing is an
  open extension point, intentionally deferred.)
- **No client-side cache invalidation logic.** Each provider owns its cache
  TTL (Anthropic's is 5 minutes by default); the harness only decides *where*
  to place breakpoints / keys, not *when* they expire.

## See also

- [ADR-0067](../../adr/0067-modular-prompt-cache-control.md) — the decision
  record for the modular policy.
- [Token accounting](token-accounting.md) — how tokens (cached or not) are
  counted, attributed as reported vs estimated, and surfaced in the report
  modal. The cache fields defined here feed that ledger.
- [Model context assembly](model-context.md) — why the system prompt is a
  single, stable, rank-ordered block (the shape that maximizes cache hits).
- [Pursuits](pursuits.md) — pursuit budgets consume these cache counts for
  cost-aware loop bounds.
