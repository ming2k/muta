# Prompt caching and cost control

> Companion to [ADR-0161](../../adr/0161-route-scoped-inference-protocols-and-prompt-cache-contracts.md)
> and [Token accounting](token-accounting.md).

Prompt caching reuses a stable request prefix such as the system prompt, tool
schemas, and earlier conversation turns. It can reduce latency and input cost,
but “supports caching” is not one portable API feature. Different providers
decide where the boundary lives, how long an entry survives, whether an
affinity key is accepted, and which counters are returned.

The key design rule is:

> Cache behavior belongs to a concrete provider route and model, not to a
> model family and not to protocol compatibility alone.

A third-party endpoint that accepts an OpenAI-shaped request is not thereby
guaranteed to accept every OpenAI cache extension. Muta enables a control only
when that exact preset and model declare it. Custom and undocumented relays
start unsupported.

## Four separate questions

Caching becomes easier to reason about when four concerns stay separate:

1. **Protocol encoding** defines where a standard field belongs in a request.
2. **Provider capability** says whether this route and model accept that field.
3. **Request preference** selects among capabilities the route actually has.
4. **Telemetry normalization** reads the provider's hit, write, and miss
   counters without changing request behavior.

The API protocol owns syntax. The provider capability owns availability and
defaults. A response counter never proves that a matching request control is
available.

## Cache modes

| Mode | Boundary owner | Client behavior |
|------|----------------|-----------------|
| `implicit` | Provider | Send no boundary marker; the provider caches eligible prefixes. Optional protocol controls such as retention or affinity are sent only when declared. |
| `automatic` | Provider API | Ask the API to advance its cache boundary automatically. Anthropic represents this with top-level `cache_control`. |
| `explicit` | Client | Mark concrete stable content boundaries. OpenAI and Anthropic use different block fields, so each protocol encoder owns its projection. |
| `disabled` | Client, when enforceable | Omit or send the provider's disabling control. It is valid only when the route declares that disabling can actually be enforced. |

`provider_default` is a preference, not a fifth mode. It resolves to the
default explicitly declared by the route. Declaration order has no semantic
effect.

Request ephemerality is also independent. A title or compaction request does
not silently override cache policy; callers that require no cache writes must
request `disabled`, and resolution fails when the provider cannot guarantee
it.

## Retention and affinity

Retention values are exact provider controls, not approximate durations. Muta
models in-memory, 5-minute, 30-minute, 1-hour, and 24-hour values separately
and rejects a value the route does not advertise.

An affinity or routing key is another independent capability. It may group
similar prefixes or isolate conversations, but it does not itself enable
caching. A route receives the session key only when it explicitly supports the
corresponding wire field.

## Protocol-standard versus provider-specific

The distinction the implementation uses is:

- OpenAI cache options, retention, affinity keys, explicit breakpoints, and
  cached-token response fields are part of OpenAI API generations. Which model
  generation supports which subset is provider/model capability data.
- Anthropic `cache_control`, its TTL values, breakpoint limit, and read/write
  counters are Anthropic Messages API semantics. A compatible relay does not
  inherit them automatically.
- Google implicit caching and explicit cached-content resources are Gemini API
  semantics. Muta currently declares only implicit caching because it does not
  implement the explicit resource lifecycle end to end.
- DeepSeek's automatic disk cache and hit/miss counters are provider-specific
  behavior on its API, even when the surrounding request is OpenAI-shaped.
- Moonshot/Kimi's top-level cached-token counter is provider-specific
  telemetry. It does not justify an undocumented affinity control.

Provider dialects are separate again: ChatGPT/Copilot headers and Antigravity's
envelope affect authentication and framing, but do not become generic cache
standards for the underlying protocol.

## Resolution and failure behavior

For every request, Muta merges the request preference over the saved route
preference, then validates the result against the route capability contract.
The result is one of unsupported, disabled, or a fully specified enabled plan.

Invalid requests fail before network dispatch. There is no fallback from
explicit to implicit, from a requested TTL to a provider default, or from an
unknown protocol to OpenAI. This makes configuration errors observable and
prevents accidental billing or payload changes.

## Telemetry

Provider response fields normalize into three independent counters:

| Counter | Meaning |
|---------|---------|
| Cache read | Input tokens served from a cache. |
| Cache write | Input tokens used to populate a cache when the provider reports them. |
| Cache miss | Provider-reported input tokens that missed. This is diagnostic and already part of prompt input, not additional usage. |

Known response locations include OpenAI Chat Completions
`prompt_tokens_details.cached_tokens`, OpenAI Responses
`input_tokens_details.cached_tokens`, Anthropic read/write fields, Google
`cachedContentTokenCount`, Moonshot/Kimi `cached_tokens`, and DeepSeek's
hit/miss fields. One normalization point handles these shapes so a new
provider-specific path cannot quietly diverge across protocol adapters.

## Extending safely

When adding a provider or model generation:

1. Select one exact inference protocol and, if necessary, one typed dialect.
2. Verify cache behavior from first-party provider documentation.
3. Declare only the modes, retention values, limits, affinity control, and
   counters that route supports.
4. Add encoder tests for every declared request control and parser tests for
   every declared counter.
5. Leave caching unsupported when evidence or end-to-end implementation is
   incomplete.

## See also

- [Providers](../../reference/providers.md)
- [Configuration](../../reference/configuration.md)
- [Token accounting](token-accounting.md)
- [Model context assembly](model-context.md)
