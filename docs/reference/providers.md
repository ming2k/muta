# Providers

Muta separates concepts that are often conflated:

- A **wire protocol** is the exact inference API request and event shape.
- A **provider dialect** is provider-specific authentication, headers, or an
  envelope layered on one wire protocol.
- A **model provider** is the upstream service surface that serves models:
  endpoint family, wire dialect, and model universe (ADR-0201). It is named by
  a `provider` id and owns the model existence and capability facts.
- A **connection** is a named pipe to exactly one model provider: it binds a
  credential and a client identity, and may narrow or override the provider's
  model set. Its identity is its `name`; it has no `id`.
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

These names are used by model metadata, model providers, connection state, and
add/edit requests. No alias is accepted and an unknown value does not fall back
to OpenAI.

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

## Model provider routes

A **model provider** is the service surface a connection points at (ADR-0201):
its identity is the triple *endpoint family, wire dialect, model universe* — never
a wire protocol and never an authentication mode. The closed id set lives in
`muta_contracts::model_providers::MODEL_PROVIDER_IDS`.

| Model provider id | Default protocol | Dialect/routing | Authentication |
|-------------------|------------------|-----------------|----------------|
| `openai` | `openai-chat-completions` | standard | API key |
| `openai-subscription` | `openai-responses` | ChatGPT/Codex | ChatGPT OAuth |
| `anthropic` | `anthropic-messages` | standard | API key |
| `google` | `google-generate-content` | Generative Language | API key |
| `google-antigravity` | `google-generate-content` | Antigravity | Google OAuth |
| `github-copilot` | Advertised per model: `openai-chat-completions`, `openai-responses`, or `anthropic-messages` | matching Copilot dialect | GitHub device OAuth |
| `xai` | `openai-chat-completions` | standard | xAI OAuth or `XAI_API_KEY` |
| `deepseek` | `openai-responses` | standard | API key |
| `glm-cn` | `openai-chat-completions` | standard plus ZCode identity | coding-plan key |
| `kimi-code` | `openai-chat-completions` | standard | coding-plan key |
| `opencode-go` | Selected per model: chat-completions, messages, or generate-content | standard relay routes | API key |
| `custom` | `openai-chat-completions` (connection default; a connection may override) | standard | optional API key |

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
| Other OpenAI provider models | unsupported | none | none |
| Anthropic provider models | automatic, 5 minutes | automatic or explicit; 5-minute or 1-hour TTL; disable; at most 4 breakpoints | reads and writes |
| Google provider models | implicit | none | reads |
| DeepSeek provider models | implicit | none | provider-specific hits and misses |
| Kimi Code models | implicit | none | provider-specific reads |
| xAI, ChatGPT subscription, Copilot, GLM CN, OpenCode Go, Antigravity, and `custom` routes | unsupported | none | none declared |

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
in `$XDG_CONFIG_HOME/muta/credentials.toml`, keyed by connection **name**.

A connection is a named pipe, not a provider definition. It declares exactly
one `provider` (a model provider id from the closed set above), owns one
credential, and declares the client identity it speaks with. It may narrow the
provider's model universe and override known capability fields, but it must not
invent a model the provider excludes — except under `provider = "custom"`,
whose universe is open by definition. `protocol`, `base_url`, and `user_agent`
are optional overrides of the provider's defaults; a connection to `custom`
supplies its own endpoint, and the default add-custom flow creates an OpenAI
Chat Completions route supporting one or more comma-separated model ids in the
Model input field.

The connection's `name` is its sole identifier (unique, compared
case-insensitively; a duplicate is rejected with a suggested alternative). It
keys the credential (`credentials.toml`), the OAuth token set (`auth.toml`),
the discovery cache, and `config.toml`'s `default_connection`. Renaming is one
atomic transaction over `credentials.toml`, `auth.toml`, and
`default_connection`; historical session and telemetry records keep the name
they ran under.

Routes are never persisted: the catalog derives each route (protocol, dialect,
endpoint, credential, capability set) at runtime from the connection's provider
plus the discovery cache. Custom routes do not inherit a provider's prompt-cache
capabilities.

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

Official providers with a remote catalog use open admission: every visible
model returned for that connection is eligible for the picker, including model
ids unknown to the installed binary. Compiled models provide startup seeds and
capability fallbacks; they do not retain a model that a successful remote
catalog no longer returns. Explicit `inject` entries remain the only way to
keep an unlisted model.

A successful, structurally valid empty catalog means the connection currently
has no available models and clears its prior list. Network, authorization,
HTTP status, and schema failures preserve the last valid result instead.

Catalog freshness and ETag validators are scoped to the complete request
identity. Changing the source, endpoint, discovery protocol, or client
emulation profile invalidates the old validator and triggers an unconditional
fetch. This prevents a catalog selected for an older client profile from being
treated as current after the profile changes.

## Adding a provider

A new model provider must declare:

1. one default wire protocol and any typed dialect;
2. exact per-model routing exceptions;
3. authentication and client identity;
4. single remote catalog source (`catalog_source`, ADR-0203);
5. an explicit prompt-cache capability record, using unsupported when the
   behavior is undocumented or not implemented end to end;
6. a stable id in `MODEL_PROVIDER_IDS` — the id names a service surface, so it
   must not encode a wire protocol (`*-compatible`) or an authentication mode
   (`*-oauth`).

See [How to add a provider](../how-to/add-a-provider.md) and
[Model metadata](model-metadata.md).
