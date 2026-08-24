# Reasoning effort

*Reasoning effort* is the per-model "how hard should it think before answering"
knob — a **depth** control, distinct from the on/off reasoning switch (see
[Model metadata](model-metadata.md#thinking-support)). Every provider muta
speaks exposes some form of it; muta abstracts them all onto one concept.

This page is the single reference for the concept, the per-provider mapping,
and how the effective ladder for a model is resolved. The implementation lives
in `crates/muta-contracts/src/effort.rs`.

## The abstraction

muta models every provider's reasoning-depth control as one
provider-independent type — the `Effort` ladder, ascending by depth:

```
none < minimal < low < medium < high < xhigh < max
```

No code outside `muta-contracts` ever sees a provider-specific depth shape: it
sees `Effort`. The protocol layer translates a chosen rung onto the **public
API specification** the channel speaks:

| API specification | wire field | form |
|-------------------|-----------|------|
| OpenAI Responses | `reasoning.effort` | enum string |
| OpenAI chat completions | `reasoning_effort` | enum string |
| Anthropic Messages | `output_config.effort` | enum string |
| Google generateContent | `thinkingConfig.thinkingLevel` / `.thinkingBudget` | enum string / int tokens |

xAI (Grok), Moonshot (Kimi), DeepSeek and Z.AI (GLM) implement the OpenAI
Responses / chat-completions specification, so they reuse the OpenAI
translation verbatim — they are not separate specs, just separate *families*
riding one.

Google is the outlier: it does not use the word "effort" or the standard
ladder. It is abstracted onto `Effort` all the same — Gemini 3.x maps a rung to
`thinkingLevel` (`minimal`/`low`/`medium`/`high`), Gemini 2.5 maps it to a
`thinkingBudget` token bucket.

### Effort vs thinking

`effort` is **depth only** and is orthogonal to the reasoning on/off switch
(`thinking`, an Anthropic/DeepSeek concept). Setting effort does not turn
thinking on, and turning thinking on does not set a depth. See
[Model metadata: thinking support](model-metadata.md#thinking-support).

## How a model's ladder is resolved

A model honors only a subset of the rungs above — its *ladder*. The effective
ladder for a channel is resolved through one precedence chain (ADR-0065):

```
live discovery (a fitting-enabled provider's GET /models)
       │   only Kimi K3 and Copilot advertise tiers here
       ▼
static baseline   ←   the EFFORT_* consts, the compiled-in fallback
       │
       ▼
&[]   (non-reasoning model, or a protocol with no depth field)
```

**Live discovery is authoritative when the upstream advertises; the baseline is
the fallback otherwise.** This matters because providers differ sharply in what
their `/models` returns:

| Provider | `/models` advertises effort tiers? | Baseline role |
|----------|:---:|----------------|
| Moonshot Kimi K3 | ✅ `think_efforts.valid_efforts` | pre-fetch seed (refreshed live) |
| GitHub Copilot | ✅ `supports.reasoning_effort` | pre-fetch seed (refreshed live) |
| OpenAI / xAI / DeepSeek / Z.AI / Google | ❌ bare `{id, object, owned_by}` list | **the effective ladder** (from prose docs) |
| Anthropic-compatible relay (unknown model) | ❌ | conservative `EFFORT_COMMON` |

So for most providers the compiled-in baseline *is* the ladder — there is no
live value to read. DeepSeek is a clear example: its `/models` returns only an
id list, so its `low`/`high`/`max` ladder is sourced from the chat-completions
request-schema enum in its docs, not from any runtime call. Kimi K3, by
contrast, advertises the same set live, so its baseline is just the seed before
the first fetch.

### An open vocabulary, not a closed one

The seven rungs above are the **vocabulary** — the words providers actually use.
They are not a ceiling. A provider may advertise a tier the vocabulary does not
name (a future `"turbo"`, say). Such a tier is **preserved**, not dropped:

- The static registry (`Model`, `Copy`) holds the vetted `[Effort]` vocabulary —
  only known rungs, compile-time.
- The runtime, per-channel view (`ModelCapabilities`) holds `[EffortLevel]`,
  where `EffortLevel` is either `Known(Effort)` or `Other(String)`. A
  live-advertised tier outside the vocabulary rides through as `Other` and is
  stamped onto the wire verbatim.

This split is deliberate: the `Copy` static registry cannot hold heap strings,
but the `Clone` runtime view can — so openness lives exactly where live
discovery lands, and the vetted baseline stays cheap and closed. A provider
adding a tier needs no muta release for that tier to reach the wire; it only
needs a release to gain a *ranked* clamp and a picker caption.

### Clamp semantics

A requested rung a model does not support is clamped, never sent raw (the
upstream would 400). `clamp_to` snaps the request **down** to the highest
supported rung ≤ the request (e.g. `max` on a model that tops out at `high`
becomes `high`). When nothing supported ranks that low, it snaps **up** to the
ladder's shallowest tier — the ladder is authoritative, so emitting an
unsupported value is never an option (Kimi K3's `low`/`high`/`max` ladder
clamps a legacy `medium` override up to `low`).

## Configuring it

Effort is set per **route** — one `(instance, model)` pair — as a string,
stored in the discovery cache (`route_settings`) and edited from the model `e`
picker in the TUI:

```toml
# $XDG_CACHE_HOME/muta/models_discovery.json
# route_settings["<instance_id>"]["<model_id>"] = { "effort": "high" }
```

It is not a `config.toml` field: routes are derived at runtime, and their
reasoning knobs are user-set route facts kept with the other per-route data.
See [Configuration: per-model reasoning settings](configuration.md#per-model-reasoning-settings).
In the TUI, press `e` on a model in the provider view to cycle it.

An unset effort leaves the server default in place. A set effort is clamped to
the resolved model's ladder at request-build time.

## chat completions vs Responses

muta prefers the newer OpenAI **Responses** API over chat completions
wherever the upstream supports it (OpenAI, xAI, DeepSeek, ChatGPT/Copilot
OAuth). Both expose the same `effort` control — `reasoning.effort` (Responses)
vs `reasoning_effort` (chat) — so the abstraction is unaffected by which
transport a channel uses. Providers that only implement the chat-completions
specification (Kimi, Z.AI, opencode-go relay) stay on chat; that is a property
of their API, not an unfinished migration. See
[Providers](providers.md) for which protocol each provider speaks.
