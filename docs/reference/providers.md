# Providers

The agent talks to LLM providers through the `Provider` trait
(`crates/platform/neenee-core/src/capability.rs`). Every provider implementation lives
in `crates/providers/neenee-providers/src/`. Provider selection happens at startup and
on `/provider` (the picker) in `apps/code/neenee-code/src/main.rs`.

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
| `OpenAiCompatProvider` | yes | yes | yes | `neenee-ai-sdk-openai` |
| OpenAI-compatible registry presets | yes | yes | yes | `OpenAiProviderSpec` (delegates to `OpenAiCompatProvider`) |
| `ResponsesProvider` (`OpenAiResponses`) | yes | yes | yes | `neenee-ai-sdk-openai` |
| `AnthropicMessagesProvider` (`Anthropic`) | yes | yes | yes | `neenee-ai-sdk-anthropic` |
| `GoogleProvider` (`GeminiNative`) | yes | no | yes | `neenee-ai-sdk-google` |
| `MockProvider` | no | no | no | `neenee-providers/src/mock.rs` |

The two OpenAI-compatible presets in `OPENAI_PROVIDER_SPECS` (`kimi-code`,
`zai-code`) are built by `OpenAiProviderSpec::build`, which returns an
`OpenAiCompatProvider` with its `id` field set to the preset identifier. They
therefore inherit every capability of `OpenAiCompatProvider`. Multi-model
catalog entries (`deepseek`, `openai`) are materialized the same way from the
catalog layer, not the preset table. `GoogleProvider` is a standalone
Gemini-native adapter: it converts the same internal tool schemas into Gemini
`functionDeclarations`, parses `functionCall` parts, and replays tool results
as `functionResponse` parts. `AnthropicMessagesProvider` speaks the
Anthropic `/messages` wire format; `ResponsesProvider` speaks the OpenAI
Responses API used by the ChatGPT subscription backend.

## Provider catalog

`default_provider` in `config.toml` is the **fresh-session default**: the
provider a new launch lands on. The `/provider` picker accepts the same names
and, on a switch, persists the choice back to `default_provider` so the next
launch follows it; see [Dual-write provider/model
selection](../adr/0066-dual-write-provider-selection.md). API keys may be
supplied through environment variables or `config.toml` fields; model selection
uses a separate `<NAME>_MODEL` env var.

### OpenAI-compatible presets

Each row corresponds to one entry in the `OPENAI_PROVIDER_SPECS` table in
`crates/providers/neenee-providers/src/registry.rs`. The endpoint, default model, and
env vars are data in that table, not hard-coded per struct.

| `default_provider` | Endpoint | API key env | Model env | Default / popular models |
|--------------------|----------|-------------|-----------|--------------------------|
| `kimi-code` | `https://api.kimi.com/coding/v1/chat/completions` | `MOONSHOT_API_KEY` | `MOONSHOT_MODEL` | `k3` (Kimi K3, 1M context) plus the platform's live `/models` list |
| `zai-code` | `https://api.z.ai/api/coding/paas/v4/chat/completions` | `ZAI_API_KEY` | `ZAI_MODEL` | `glm-5.2` (default), `glm-5.1`, `glm-4.7` |

### Bespoke providers

| `default_provider` | Struct | Endpoint | API key env | Model env | Default / popular models |
|--------------------|--------|----------|-------------|-----------|--------------------------|
| `openai` | `OpenAiCompatProvider` | `https://api.openai.com/v1/chat/completions` | `OPENAI_API_KEY` | `OPENAI_MODEL` | `gpt-5.6-sol` (default), `gpt-5.6-terra`, `gpt-5.6-luna`, `gpt-5.5`, `gpt-5.4`, `gpt-5.4-mini` |
| `anthropic` | `AnthropicMessagesProvider` | `https://api.anthropic.com/v1/messages` (overridable via `config.anthropic_base_url`) | `ANTHROPIC_API_KEY` | `ANTHROPIC_MODEL` | `claude-opus-4-8` (default), `claude-fable-5`, `claude-sonnet-5`, `claude-sonnet-4-6`, `claude-haiku-4-5-20251001` |
| `gemini` | `GoogleProvider` | `{gemini_base_url}/models/{model}:generateContent?key={key}` (default base `https://generativelanguage.googleapis.com/v1beta`; env `GEMINI_BASE_URL`, then `config.gemini_base_url`) | `GEMINI_API_KEY` | `GEMINI_MODEL` | `gemini-3.5-flash` (default), `gemini-3-pro-preview`, `gemini-3-flash-preview`, `gemini-3.1-pro-preview`, `gemini-2.5-flash`, `gemini-2.5-pro`, `gemini-2.0-flash` — see [`GOOGLE_BUILTIN_MODELS`](../../crates/providers/neenee-providers/src/registry.rs). Native Gemini is a **closed** model set: the add-model overlay offers only these ids, no free-text fallback. |
| `deepseek` | `OpenAiCompatProvider` | `https://api.deepseek.com/v1/chat/completions` | `DEEPSEEK_API_KEY` | `DEEPSEEK_FLASH_MODEL` / `DEEPSEEK_PRO_MODEL` | `deepseek-v4-flash`, `deepseek-v4-pro` (1M context; thinking + non-thinking modes) |

Notes:

- `deepseek` is a multi-model catalog entry: `deepseek-v4-flash` and
  `deepseek-v4-pro` share one API key (`DEEPSEEK_API_KEY`) and one endpoint.
  It is materialized by the catalog layer, not by `OPENAI_PROVIDER_SPECS`.
- `zai-code` targets the Z.AI (Zhipu) coding-plan platform and serves the
  GLM-5 family; it sends a `opencode/1.17.10` User-Agent so the platform
  recognises a coding agent.
- `kimi-code` tracks the Kimi Code platform's live `GET /models` list at
  startup (`k3` by default); model overrides are ignored. It is the first
  **fitting** template: platform-native ids the client registry does not know
  (e.g. `kimi-for-coding`) are materialized with the capability metadata the
  platform advertises — context window, reasoning, vision, effort tiers — so
  new platform models become usable with zero client changes
  ([ADR-0065](../adr/0065-runtime-fitted-model-capability-overlay.md)).
- `opencode-go` is a runtime-derived entry whose model list is built from
  `KNOWN_MODELS` at startup, spanning OpenAI- and Anthropic-compatible
  models (e.g. MiniMax, Qwen) behind opencode-go's endpoints.

## Dispatch sites

Provider construction is split across two layers. The catalog
(`build_catalog` in `crates/platform/neenee-agent/src/catalog.rs`) materializes every
provider id — registry preset, built-in multi-model entry, or user-defined
`[[providers]]` instance — into a `Channel` carrying fully resolved
credentials, model id, and transport, so startup and runtime switching share
one source of truth for the env-var-then-config resolution rules. The
concrete `Provider` is then built by `build_provider_for_channel` in
`crates/providers/neenee-providers/src/registry.rs`, which matches on `Transport`.

1. The registry presets are built from `OPENAI_PROVIDER_SPECS` via
   `OpenAiProviderSpec::build`, yielding an `OpenAiCompatProvider` with its
   `id` field set to the preset identifier.
2. Multi-model built-ins (`openai`, `google`, `deepseek`, `anthropic`,
   `kimi-code`, `zai-code`) are seeded by the legacy-instance migration in
   `migrate_legacy_provider_instances`; `opencode-go` is derived at runtime
   from `KNOWN_MODELS`. An unknown id resolves to an internal `MockProvider`
   fallback (not user-visible).

| Site | Function | Purpose |
|------|----------|---------|
| Startup dispatch | `catalog::build_provider_for` | Reads `config.default_provider`, resolves env/config values via the catalog |
| Runtime switch | `AgentRequest::SwitchProvider` handler | Resolves a TUI-entered key/url, persists the selection to `config.toml` (`default_provider`/`default_model`) **and** pins it to the session, then rebuilds via the catalog |
| API-key status | `provider_key_status` | Reports per-provider readiness to the TUI (derived from the catalog) |
| Model-name mirror | `catalog::resolved_model_name` | Friendly default model label for the TUI header |

Runtime provider switching uses `ProxyProvider`
(`crates/platform/neenee-agent/src/orchestration.rs`), an
`Arc<RwLock<Arc<dyn Provider>>>` holder that hot-swaps the active provider
without rebuilding the `Agent`.

## Retry

Transient HTTP `408`, `429`, `5xx`, connection, and timeout failures are
wrapped in `RetryableError` (`crates/platform/neenee-core/src/error.rs`) by
`ensure_success` and `transport_error` in `crates/providers/neenee-providers/src/lib.rs`.
The marker prefix
is `[NEENEE_RETRYABLE]`.

Retry is a round-level loop inside `execute_round`
(`crates/platform/neenee-agent/src/orchestration.rs`),
not a provider decorator. Configuration:

| Config key | Default | Hard maximum |
|------------|---------|--------------|
| `provider_retry_max_attempts` | `6` | `10` |
| `provider_retry_base_ms` | `1000` | — |
| `provider_retry_max_ms` | `30000` | — |

Backoff is computed by `retry_delay_ms` as exponential
`base_ms * 2^(attempt-1)` capped at `max_ms`. Server `Retry-After` or
`retry-after-ms` headers (parsed by `retry_after_ms` in
`crates/providers/neenee-providers/src/lib.rs`) take
priority. Once any tool has run in the current round, retryable errors become
terminal so tool side effects are never replayed.

## See also

- [Provider capabilities](../explanation/provider-capabilities.md) — why
  providers differ on tool and reasoning support
- [Tool rounds](../explanation/agent-design/rounds-and-turns.md) — how the universal
  fallback covers providers without native tools
- [How to add a provider](../how-to/add-a-provider.md) — implementing a new
  adapter
- [Harness architecture](../explanation/agent-design/harness.md) — provider retry and the
  harness safety bounds
