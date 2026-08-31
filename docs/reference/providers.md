# Providers

Muta separates three concepts that are often conflated:

- A **wire protocol** is the exact inference API request and event shape.
- A **provider dialect** is provider-specific authentication, headers, or an
  envelope layered on one wire protocol.
- A **route** is one connection and model using one protocol, dialect,
  endpoint, credential, and capability set.

Protocol compatibility does not grant optional provider features. In
particular, prompt-cache controls are enabled only by the concrete route's
capability declaration.

## Implemented inference protocols

The canonical protocol names are a closed set:

| Canonical name | Request surface | Streaming surface |
|----------------|-----------------|-------------------|
| `openai-chat-completions` | OpenAI Chat Completions `messages` request | Chat completion chunks |
| `openai-responses` | OpenAI Responses `instructions` and `input` items | `response.*` events |
| `anthropic-messages` | Anthropic Messages `system`, `messages`, and content blocks | Anthropic message/content-block events |
| `google-generate-content` | Google `generateContent` contents and parts | `streamGenerateContent` candidates and parts |

These names are used by model metadata, provider presets, custom connection
state, and add/edit events. No alias is accepted and an unknown value does not
fall back to OpenAI.

All four adapters support native tool declarations and structured streaming.
Reasoning support is resolved per model and route rather than inferred from the
adapter name.

## Provider dialects

| Protocol | Dialects | Difference from the standard dialect |
|----------|----------|--------------------------------------|
| OpenAI Chat Completions | standard, Copilot | Copilot bearer and client headers |
| OpenAI Responses | standard, ChatGPT, Copilot | Subscription authentication, account/client headers, and non-persistent response state |
| Anthropic Messages | standard, Copilot | Copilot bearer and client headers instead of Anthropic API-key headers |
| Google generateContent | Generative Language, Antigravity | Antigravity `v1internal` envelope, project identity, and response normalization |

Dialects are mutually exclusive typed values. For example, one Responses route
cannot accidentally be both ChatGPT and Copilot.

## Preset routes

| Preset id | Inference protocol | Dialect/routing | Authentication |
|-----------|--------------------|-----------------|----------------|
| `openai` | OpenAI Chat Completions | standard | API key |
| `anthropic` | Anthropic Messages | standard | API key |
| `google` | Google generateContent | Generative Language | API key |
| `deepseek` | OpenAI Responses | standard | API key |
| `xai-oauth` | OpenAI Chat Completions | standard | xAI OAuth |
| `chatgpt-oauth` | OpenAI Responses | ChatGPT | ChatGPT OAuth |
| `copilot-oauth` | Advertised per model: Chat Completions, Responses, or Messages | matching Copilot dialect | GitHub device OAuth |
| `kimi-code` | OpenAI Chat Completions | standard | coding-plan key |
| `zai-code` | OpenAI Chat Completions | standard plus ZCode identity | coding-plan key |
| `opencode-go` | Selected per model: Chat Completions, Messages, or Google generateContent | standard relay routes | API key |
| `antigravity-oauth` | Google generateContent | Antigravity | Google OAuth |
| `custom-openai` | OpenAI Chat Completions | standard | optional API key |

Copilot's live model catalogue is authoritative for the protocol of each model.
A Copilot model advertising an unsupported Google protocol is rejected rather
than projected onto another API. OpenCode Go uses the registered baseline
protocol for each model to choose its relay endpoint.

## Prompt-cache capability matrix

This table describes the implemented and declared behavior, not everything an
upstream service may offer.

| Route/model | Default | Selectable controls | Telemetry |
|-------------|---------|---------------------|-----------|
| OpenAI GPT-5.6 family | implicit, 30 minutes | implicit or explicit; 30-minute TTL; affinity key; at most 4 explicit breakpoints | reads and writes |
| OpenAI GPT-5.5 family | implicit | optional 24-hour retention; affinity key | reads |
| OpenAI GPT-5.4, GPT-5.4 mini, and GPT-5.2 variants | implicit | in-memory or 24-hour retention; affinity key | reads |
| OpenAI GPT-4o, GPT-4o mini, GPT-5.3 Codex Spark | implicit | in-memory retention; affinity key | reads |
| Other OpenAI preset models | unsupported | none | none |
| Anthropic preset models | automatic, 5 minutes | automatic or explicit; 5-minute or 1-hour TTL; disable; at most 4 breakpoints | reads and writes |
| Google preset models | implicit | none | reads |
| DeepSeek preset models | implicit | none | provider-specific hits and misses |
| Kimi Code models | implicit | none | provider-specific reads |
| xAI, ChatGPT, Copilot, ZAI, OpenCode Go, Antigravity, and custom routes | unsupported | none | none declared |

“Unsupported” means Muta sends no cache control and rejects a non-default cache
preference for that route. It does not claim that the upstream never performs
internal caching.

OpenAI and Anthropic request fields are encoded by their protocol adapters,
but availability remains provider/model data. DeepSeek hit/miss and Kimi
cached-token fields are provider-specific telemetry. Google explicit cached
content is intentionally not declared because Muta does not implement the
resource create/reference/delete lifecycle.

See [Prompt caching and cost control](../explanation/agent-design/prompt-caching.md)
for the classification rules and [ADR-0161](../adr/0161-route-scoped-inference-protocols-and-prompt-cache-contracts.md)
for the decision.

## Connections and route derivation

Connections live in `$XDG_STATE_HOME/muta/connections.toml`. Credentials live
in `$XDG_CONFIG_HOME/muta/credentials.toml`. A preset connection stores its
preset id, identity, and credential reference; its model routes are derived at
runtime from the preset and live discovery cache.

A pure-custom connection stores an exact `protocol`, endpoint, and model list.
The default add-custom flow creates an OpenAI Chat Completions route, supporting
one or more comma-separated models in the Model input field, while the state
schema can represent any of the four canonical protocols. Custom routes
do not inherit a preset's prompt-cache capabilities.

Credential resolution is `api_key_env` first, then the connection entry in
`credentials.toml`. OAuth connections resolve their current bearer from the
auth store.

## Model discovery

Inference and discovery protocols are distinct. Both OpenAI inference
protocols use the OpenAI `/models` discovery shape; Anthropic and Google use
their own model-list surfaces. ChatGPT uses the Codex model catalogue.

Discovery facts are scoped to the connection. Remote protocol metadata may
override a model's baseline route only for that connection, which is how
Copilot can serve the same model id over different APIs on different plans.

## Adding a provider

A new preset must declare:

1. one default wire protocol and any typed dialect;
2. exact per-model routing exceptions;
3. authentication and client identity;
4. trusted discovery/fitting behavior;
5. an explicit prompt-cache capability record, using unsupported when the
   behavior is undocumented or not implemented end to end.

See [How to add a provider](../how-to/add-a-provider.md) and
[Model metadata](model-metadata.md).
