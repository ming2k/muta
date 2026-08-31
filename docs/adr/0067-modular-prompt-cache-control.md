# ADR-0067: Modular prompt-cache control policy

- Status: Superseded by ADR-0161
- Date: 2026-07-17

## Context

Prompt caching is the dominant cost lever for a multi-turn agent: a cached
prefix is billed at ~0.1× input (Anthropic) or folded into a discount (OpenAI,
Gemini, Moonshot). Different providers expose wildly different surfaces:

- **Anthropic** — the client stamps explicit `cache_control: {"type":"ephemeral"}`
  breakpoints and the response reports separate write (`cache_creation`) and
  read (`cache_read`) counters. neenee already did this well.
- **Moonshot / Kimi** — the client supplies a session-scoped
  `prompt_cache_key`; the response surfaces `cached_tokens` (read only).
- **OpenAI / Gemini** — the server auto-caches with no client control; the
  discount surfaces as `cached_tokens` / `cachedContentTokenCount`.

Before this change neenee surfaced cache counts **only** for Anthropic. OpenAI,
Gemini, and the Moonshot `prompt_cache_key` were all unimplemented — the OpenAI
response parser even documented the `cached_tokens` field and then deliberately
ignored it ("its caching is invisible by design"). That forfeited both real
discounts (Moonshot session key) and accurate cost attribution (every provider's
reported hit rate).

This was a comparison finding against the sibling `kimi-code` project, which
parses every provider's cache field and wires the Moonshot session key.

## Decision

Introduce a single, **pure-domain** cache-policy classifier and a shared
read helper, then route every provider through them.

1. **`CachePolicy` enum** (`neenee-core::cache`) classifies a model family into
   one of three strategies — `Breakpoints` (Anthropic), `SessionKey`
   (Moonshot/Kimi), `Automatic` (OpenAI/Gemini/everything else) — resolved from
   the model's `family`. Predicates (`stamps_breakpoints`, `injects_session_key`)
   let request builders branch without knowing provider specifics.

2. **`read_cached_tokens`** — one helper that finds the cache-read discount
   wherever the provider hides it (`cached_tokens`, `prompt_tokens_details
   .cached_tokens`, `cachedContentTokenCount`). The OpenAI chat-completions,
   OpenAI Responses, and Gemini response parsers now fold it into
   `TokenUsage::cache_read_input_tokens`, so the token-source report shows the
   hit rate for all providers, not just Anthropic. `cache_creation` stays zero
   for the auto-cachers (no separate write counter exists).

3. **Session-key injection** — `OpenAiCompatProvider` gains an optional
   `prompt_cache_key`; `build_provider_for_channel` resolves the policy and
   stamps the **session id** as the key when the family is `SessionKey`. The
   session id is read from `Agent::thread_id()` inside the `activate`/model-
   switch path, so no new parameter ripples through every dispatch arm.

The Anthropic breakpoint stamping (already correct) is untouched; this ADR adds
the other two strategies and the shared accounting.

## Consequences

- Every provider's cache discount is now surfaced in the token-source report,
  not just Anthropic's. The cost model is honest.
- Moonshot/Kimi sessions reuse a server-side cache namespace per conversation,
  reducing repeated-prefix billing — the same win `kimi-code` gets.
- The classifier is the single place to teach the agent a new family's caching
   strategy; request/response adapters stay mechanical.
- The session id now crosses into provider construction (`build_provider_for_
  channel` gained an `Option<&str>`). It is `None` at shared bootstrap (pre-
  session) and `Some` at session create / model switch.

See also ADR-0068 (system-reminder injection) and ADR-0069 (pursuit budgets),
both of which consume the cache accounting for cost-aware behaviour.
