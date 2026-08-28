# Providers

The agent talks to LLM providers through the `Provider` trait
(`crates/muta-contracts/src/capability.rs`). Every provider implementation lives
in `crates/muta-providers/src/`. Provider selection happens at startup and
on `/models` (the flat model picker) in `apps/tui/crates/mutx/src/providers.rs`.

## Capability matrix

Three capability surfaces matter for tool-using agents:

- **Native tools** — the adapter serializes the `tool_specs` carried by each
  `ModelRequest` into its native request body. An adapter that ignores those
  declarations uses the universal text protocol instead.
- **Reasoning** — the provider reads `reasoning_content` from responses and
  emits `ProviderStreamEvent::ReasoningDelta`.
- **Structured streaming** — the provider implements `stream_chat_events`
  with the full event set (`TextDelta`, `ReasoningDelta`, `ToolCallDelta`).
  Providers that do not implement it fall back to the trait default, which
  only emits `TextDelta`.

| Provider | Native tools | Reasoning | Structured streaming | Source |
|----------|--------------|-----------|----------------------|--------|
| `OpenAiChatCompletionsProvider` | yes | yes | yes | `muta-llm-client` (protocol::openai) |
| OpenAI-compatible registry presets | yes | yes | yes | `OpenAiProviderSpec` (delegates to `OpenAiChatCompletionsProvider`) |
| `OpenAiResponsesProvider` (`OpenAiResponses`) | yes | yes | yes | `muta-llm-client` (protocol::openai) |
| `AnthropicMessagesProvider` (`Anthropic`) | yes | yes | yes | `muta-llm-client` (protocol::anthropic) |
| `GoogleProvider` (`Google`) | yes | no | yes | `muta-llm-client` (protocol::google) |

The two OpenAI-compatible presets in `OPENAI_PROVIDER_SPECS` (`kimi-code`,
`zai-code`) are built by `OpenAiProviderSpec::build`, which returns an
`OpenAiChatCompletionsProvider` with its `id` field set to the preset identifier. They
therefore inherit every capability of `OpenAiChatCompletionsProvider`. Multi-model
catalog entries (`deepseek`, `openai`) are materialized the same way from the
catalog layer, not the preset table. `GoogleProvider` is a standalone
Google-native adapter: it converts the same internal tool schemas into Google
`functionDeclarations`, parses `functionCall` parts, and replays tool results
as `functionResponse` parts. `AnthropicMessagesProvider` speaks the
Anthropic `/messages` wire format; `OpenAiResponsesProvider` speaks the OpenAI
Responses API used by the ChatGPT subscription backend.

## Provider catalog

`default_provider` in `config.toml` is the **fresh-session default**: the
connection a new launch lands on. The `/models` picker accepts the same
ids and, on a switch, persists the choice back to `default_provider` so the
next launch follows it; see [Dual-write provider/model
selection](../adr/0066-dual-write-provider-selection.md).

Connections are declared in the state store
(`$XDG_STATE_HOME/muta/providers.toml`, one `[[providers]]` row per
connection); each connection references a preset by `preset_id` (or is a
pure-custom declaration) and owns exactly one credential, stored in
`credentials.toml` keyed by connection id (`[providers.<id>]`, see
[Paths](paths.md)). The concrete routes (per-model transport/endpoint/
reasoning) are **derived at runtime** from the connection's preset and the
discovery cache — they are never persisted, so two connections of the same
preset cannot drift apart. Credential resolution precedence is
`api_key_env` env var > `credentials.toml`. The legacy `<PROVIDER>_API_KEY`
environment variables and `config.toml` `*_api_key` fields of earlier releases
are no longer read at runtime.

### OpenAI-compatible presets

Each row corresponds to one entry in the `OPENAI_PROVIDER_SPECS` table in
`crates/muta-providers/src/registry/mod.rs`. The endpoint, default model, and
env vars are data in that table, not hard-coded per struct.

| `default_provider` | Endpoint | Credentials | Default / popular models |
|--------------------|----------|-------------|--------------------------|
| `kimi-code` | `https://api.kimi.com/coding/v1/chat/completions` | instance credential (`credentials.toml [providers.<id>]`) | `k3` (Kimi K3, 1M context) plus the platform's live `/models` list |
| `zai-code` | `https://open.bigmodel.cn/api/coding/paas/v4/chat/completions` | instance credential (`credentials.toml [providers.<id>]`) | `glm-5.3` (default), `glm-5.3-flash`, `glm-5.2` |

### Bespoke providers

| `default_provider` | Struct | Endpoint | Credentials | Default / popular models |
|--------------------|--------|----------|-------------|--------------------------|
| `openai` | `OpenAiChatCompletionsProvider` | `https://api.openai.com/v1/chat/completions` | instance credential (`credentials.toml [providers.<id>]`) | `gpt-5.6-sol`, `gpt-5.6-terra`, `gpt-5.6-luna`, `gpt-5.5`, `gpt-5.4`, `gpt-5.4-mini` |
| `anthropic` | `AnthropicMessagesProvider` | `https://api.anthropic.com/v1/messages` (overridable via an instance `base_url`) | instance credential (`credentials.toml [providers.<id>]`) | `claude-fable-5`, `claude-sonnet-5`, `claude-opus-4-8`, `claude-sonnet-4-6`, `claude-haiku-4-5-20251001` |
| `google` | `GoogleProvider` | `{base_url}/models/{model}:generateContent?key={key}` (default base `https://generativelanguage.googleapis.com/v1beta`; overridable via an instance `base_url`) | instance credential (`credentials.toml [providers.<id>]`) | `gemini-3.7-flash`, `gemini-3.5-flash`, `gemini-3-pro-preview`, `gemini-3-flash-preview`, `gemini-3.1-pro-preview`, `gemini-2.5-flash`, `gemini-2.5-pro`, `gemini-2.0-flash` — see [`GOOGLE_BUILTIN_MODELS`](../../crates/muta-providers/src/registry/google.rs). Native Gemini is a **closed** model set: the add-model overlay offers only these ids, no free-text fallback. |
| `deepseek` | `OpenAiResponsesProvider` | `https://api.deepseek.com/v1/responses` | instance credential (`credentials.toml [providers.<id>]`) | `deepseek-v4-flash`, `deepseek-v4-flash-0731`, `deepseek-v4-pro`, `deepseek-v4-pro-0813` (1M context; thinking + non-thinking modes) |

Notes:

- `deepseek` is a multi-model preset connection: `deepseek-v4-flash` and
  `deepseek-v4-pro` share one credential and one endpoint. It is derived by
  the catalog from the `deepseek` preset, not by `OPENAI_PROVIDER_SPECS`.
  Both V4 models natively speak the OpenAI **Responses API** (Flash since the
  0731 GA, Pro since 0813), so the preset's routes use the Responses
  transport. The dated ids (`-0731` / `-0813`) pin a snapshot; the bare ids
  float with the upstream latest.
- `zai-code` targets the Zhipu BigModel / Z.AI coding-plan platform (CN) and serves the
  GLM-5 family; it sends a `ZCode/3.5.3` User-Agent along with native ZCode identity headers
  (`X-Title`, `X-ZCode-Agent`, `HTTP-Referer`) so the platform recognises its native coding client.
  It tracks the platform's live `GET /models` list at startup (ids only — the endpoint returns
  no capability metadata), intersected against the client's GLM baselines; `glm-5.3` is the
  default, `glm-5.3-flash` (native multimodal, ~1/3 credit burn) and `glm-5.2` round out the
  curated offering list.
- `kimi-code` tracks the Kimi Code platform's live `GET /models` list at
  startup (`k3` by default); model overrides are ignored. It is the first
  **fitting** preset: platform-native ids the client registry does not know
  (e.g. `kimi-for-coding`) are materialized with the capability metadata the
  platform advertises — context window, reasoning, vision, effort tiers — so
  new platform models become usable with zero client changes
   ([ADR-0065](../adr/0065-runtime-fitted-model-capability-overlay.md)).
- `copilot-oauth` tracks GitHub Copilot's authenticated `/models` list. Only
  `model_picker_enabled` models are selectable; each model uses the endpoint it
  advertises (`/chat/completions`, `/responses`, or `/v1/messages`) and its
  provider-scoped limits and capabilities. See [Model Metadata](model-metadata.md).
  The login flow uses the public Copilot OAuth App client id shared with
  opencode; a self-registered OAuth App is likely to receive only the
  always-available GPT-4o family because GitHub filters models by client id.
  See [Copilot Provider Pitfalls](../how-to/copilot-provider-pitfalls.md).
- `opencode-go` is a runtime-derived entry whose model list is built from
  its local baseline table at startup, spanning OpenAI- and
  Anthropic-compatible models (e.g. MiniMax, Qwen) behind opencode-go's
  endpoints.

### OAuth and subscription providers

Provider presets that authenticate with a browser OAuth flow instead of an
API key (the `oauth` module in `muta-providers`; tokens persist in
`auth.toml` — see [Paths](paths.md)). Added from the TUI's preset-connection
flow;
the `/models` picker accepts them like any other provider.

| Template id | Backend | Notes |
|-------------|---------|-------|
| `xai-oauth` | xAI SuperGrok subscription | OAuth2 (PKCE + device code) against xAI; serves the Grok family ([ADR-0052](../adr/0052-xai-supergrok-provider.md)) |
| `chatgpt-oauth` | ChatGPT subscription | Browser PKCE login against `auth.openai.com` (device-code grant available as the headless fallback); inference over the Codex Responses backend at `chatgpt.com/backend-api/codex/responses`. Tracks the account's live `/backend-api/codex/models` catalog (ETag-revalidated, 5-minute cache TTL) with capability fitting; the compiled snapshot (`gpt-5.6-sol`…`gpt-5.3-codex-spark`) is the offline fallback |
| `copilot-oauth` | GitHub Copilot subscription | Public Copilot OAuth App client id (shared with opencode); tracks the plan-unlocked model list with live discovery + fitting. See [Copilot Provider Pitfalls](../how-to/copilot-provider-pitfalls.md) |

### sub2api relay presets

Templates for sub2api-style relays that forward another vendor's protocol —
configured with the relay's `base_url` and an API key, exactly like the
built-ins:

| Template id | Protocol | Notes |
|-------------|----------|-------|
| `anthropic-sub2api` | `anthropic` | Anthropic-format relay; live discovery surfaces the relay's Claude set |
| `openai-sub2api` | `openai` | OpenAI-format relay |
| `antigravity-sub2api` | `google` | Google-native relay (Antigravity/Atmosphere family) |

See [How to use sub2api relays](../how-to/use-sub2api.md).

### Custom connections

The generic escape hatch for any OpenAI-compatible endpoint the curated
presets do not cover — third-party relays, self-hosted gateways, or
subscription bundles that expose a `/v1/chat/completions` surface:

The TUI exposes this as `Connections › Add custom connection`, separate from
the preset chooser. The editor shows a free-text Model field (registry-known
OpenAI ids as suggestions, plus the raw typed id as a custom value); the typed
id becomes the connection's declared model. New custom connections have no
`preset_id` and no live discovery, so the connection keeps exactly the id the
user typed. Existing `preset_id = "custom-openai"` declarations remain
compatible.

Model ids travel **verbatim**: an endpoint with case-sensitive ids (e.g. the
WeChat OpenAI-compatible endpoint serves `GLM-5.2` / `Deepseek-v4-flash` and
rejects the lowercase spellings) works because nothing normalizes the id. The
cased WeChat ids carry registered baseline metadata (200K context); ids the
registry does not know resolve through the conservative fallback.

## Dispatch sites

Provider construction is split across two layers. The catalog
(`build_catalog` in `crates/muta-agent/src/catalog.rs`) materializes every
provider id — registry preset, built-in multi-model entry, or user-defined
`[[providers]]` instance — into a `Channel` carrying fully resolved
credentials, model id, and transport, so startup and runtime switching share
one source of truth for the env-var-then-config resolution rules. The
concrete `Provider` is then built by `build_provider_for_channel` in
`crates/muta-providers/src/registry/`, which matches on `Transport`.

1. The registry presets are built from `OPENAI_PROVIDER_SPECS` via
   `OpenAiProviderSpec::build`, yielding an `OpenAiChatCompletionsProvider` with its
   `id` field set to the preset identifier.
2. Multi-model built-ins (`openai`, `google`, `deepseek`, `anthropic`,
   `kimi-code`, `zai-code`) are seeded by the legacy-instance migration in
   `migrate_legacy_provider_instances`; `opencode-go` is derived at runtime
   from its baseline table. An unknown id resolves to a `NoProvider`
   sentinel (not user-visible; chat dispatch refuses up-front).

| Site | Function | Purpose |
|------|----------|---------|
| Startup dispatch | `catalog::build_provider_for` | Reads `config.default_provider`, resolves env/config values via the catalog |
| Runtime switch | `AgentRequest::SwitchProvider` handler | Resolves a TUI-entered key/url, persists the selection to `config.toml` (`default_provider`/`default_model`) **and** pins it to the session, then rebuilds via the catalog |
| API-key status | `provider_key_status` | Reports per-provider readiness to the TUI (derived from the catalog) |
| Model-name mirror | `catalog::resolved_model_name` | Friendly default model label for the TUI header |

Runtime provider switching uses `ProxyProvider`
(`crates/muta-agent/src/orchestration.rs`), an
`Arc<RwLock<Arc<dyn Provider>>>` holder that hot-swaps the active provider
without rebuilding the `Agent`.

## Retry

Transient HTTP `408`, `429`, `5xx`, connection, and timeout failures are
wrapped in `RetryableError` (`crates/muta-contracts/src/error.rs`) by
`ensure_success` and `transport_error` in `crates/muta-providers/src/lib.rs`.
The marker prefix
is `[MUTA_RETRYABLE]`.

Retry is a round-level loop inside `execute_round`
(`crates/muta-agent/src/orchestration.rs`),
not a provider decorator. Configuration:

| Config key | Default | Hard maximum |
|------------|---------|--------------|
| `provider_retry_max_attempts` | `30` | `60` |
| `provider_retry_base_ms` | `1000` | — |
| `provider_retry_max_ms` | `10000` | — |

Backoff is computed by `retry_delay_ms` as exponential
`base_ms * 2^(attempt-1)` capped at `max_ms`. Server `Retry-After` or
`retry-after-ms` headers (parsed by `retry_after_ms` in
`crates/muta-providers/src/lib.rs`) take
priority. Once any tool has run in the current round, retryable errors become
terminal so tool side effects are never replayed.

## See also

- [Provider capabilities](../explanation/provider-capabilities.md) — why
  providers differ on tool and reasoning support
- [Rounds and turns](../explanation/agent-design/rounds-and-turns.md) — how the universal
  fallback covers providers without native tools
- [How to add a provider](../how-to/add-a-provider.md) — implementing a new
  adapter
- [Harness architecture](../explanation/agent-design/harness.md) — provider retry and the
  harness safety bounds
