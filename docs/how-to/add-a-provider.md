# How to add a provider

This guide walks through wiring a new LLM provider into muta. For the
existing provider matrix, see [Providers](../reference/providers.md). For the
capability model that decides which path to take, see
[Provider capabilities](../explanation/provider-capabilities.md).

muta resolves every provider through one catalog
(`build_catalog` in `crates/muta-agent/src/catalog/`): it derives the
concrete routes (per-model transport/endpoint/credential/reasoning) from each
provider instance's template plus the discovery cache, then constructs the
concrete `Provider` via `build_provider_for_channel` in
`crates/muta-providers/src/registry/mod.rs`.
Startup and a `/models` pick share this single path — there is no separate
dispatch `match` to edit for presets or user entries.

## Choose a path

| Provider speaks... | Path | Effort |
|--------------------|------|--------|
| OpenAI Chat Completions, or any endpoint reachable with a URL + key | **Custom OpenAI template** in the TUI (`/connections` → `＋ Add connection`), or a user-defined entry in `config.toml` | None (no code) |
| OpenAI Chat Completions, and you want it shipped as a built-in | Per-provider file in `registry/` | Small |
| A genuinely incompatible contract (different roles, no `tools` field) | Standalone adapter | Medium |

Prefer the config path for private or self-hosted endpoints, and the registry
path for a vendor preset every muta user would want.

## Path 0: The Custom OpenAI template (no code, no config editing)

For any OpenAI-compatible endpoint — a third-party relay, a self-hosted
gateway, a subscription bundle exposing `/v1/chat/completions` — pick
**Custom OpenAI** in `/connections` → `＋ Add connection`. The form asks for
Name, Base URL (the full chat-completions URL), Token, and **Model**: the
model field offers the registry-known OpenAI ids as fuzzy suggestions plus
the raw typed id as a custom value, so the exact id the endpoint expects
becomes the seeded channel.

Two properties worth knowing:

- **Model ids are case-sensitive and travel verbatim.** Some relays serve
  cased ids (`GLM-5.2`, not `glm-5.2`); nothing in the editor, the config
  layer, or the request builder normalizes the id.
- **The instance is never re-seeded.** Unlike curated templates there is no
  model snapshot to mirror, so a later startup never replaces the typed id.

The equivalent hand-written state looks like this (see [Path 1](#path-1-user-defined-entry-no-code)
for the full field reference). Instances live in `providers.toml`
(`$XDG_STATE_HOME/muta/`), credentials in `credentials.toml`, and only the
selection (`default_provider` / `default_model`) in `config.toml`:

```toml
# $XDG_CONFIG_HOME/muta/config.toml — behavior only
default_provider = "wechat"
default_model = "GLM-5.2"
```

```toml
# $XDG_STATE_HOME/muta/providers.toml — instances
[[providers]]
id = "wechat"
name = "WeChat OpenAI"
transport = "OpenAi"
base_url = "https://chatapi.weixin.qq.com/openai/v1/chat/completions"
models = ["GLM-5.2"]
```

```toml
# $XDG_CONFIG_HOME/muta/credentials.toml — secrets
[providers]
wechat = "sk-..."
```

## Path 1: User-defined entry (no code)

Any OpenAI-compatible, Google-native, or Anthropic-format endpoint can be
added to `providers.toml` without touching code. Declare a pure-custom
instance (no `template_id`) with its transport, endpoint, and model ids:

```toml
[[providers]]
id = "acme"
name = "Acme"
transport = "OpenAi"          # OpenAi | OpenAiResponses | Anthropic | Google
base_url = "https://api.acme.example/v1/chat/completions"
# api_key_env = "ACME_API_KEY"   # optional env var holding the credential
models = ["acme-1"]
```

```toml
[providers]
acme = "sk-..."               # $XDG_CONFIG_HOME/muta/credentials.toml
```

A **native-Google relay / 中转站** uses `Google`. The `base_url` is the
versioned base (carry the `/v1beta` prefix — the `/models/{id}:generateContent`
path is appended for you). Auth stays on the `?key=` query param:

```toml
[[providers]]
id = "my-gemini-relay"
name = "My Google Relay"
transport = "Google"
base_url = "https://relay.example.com/v1beta"
models = ["gemini-2.5-flash"]
```

To redirect the **built-in** `google` template instead (so picking `google` in
`/models` and `default_provider = "google"` route through the relay), create
an instance referencing the template with a `base_url` override — the override
wins over the template's default endpoint:

```toml
[[providers]]
id = "google"
name = "Google"
template_id = "google"
base_url = "https://relay.example.com/v1beta"
```

Instance fields:

| Field | Meaning |
|-------|---------|
| `id` | Unique instance id; referenced by `default_provider` and by `credentials.toml` |
| `name` | Display name; defaults to the id |
| `template_id` | Optional: derive routes from a template (`deepseek`, `kimi-code`, `google`, ...). Pure-custom instances omit it and declare `transport` / `base_url` / `models` below |
| `auth` | `ApiKey` (default), or an OAuth variant for subscription instances |
| `api_key_env` | Optional env var *name* holding the credential; wins over `credentials.toml` |
| `transport` | `OpenAi`, `OpenAiResponses`, `Anthropic`, or `Google` (pure-custom only) |
| `base_url` | Full chat-completions URL (OpenAI), `/responses` URL (Responses), `/messages` URL (Anthropic), or **versioned Google base** (native Google, e.g. `https://relay.example.com/v1beta` — the `/models/{id}:generateContent` path is appended for you) |
| `user_agent` | OpenAI-compatible and native Google (pure-custom only) |
| `models` | The declared model ids a pure-custom instance serves |

Per-model reasoning (`effort` / `thinking`) is **not** a persisted field — it
lives per `(instance, model)` in the discovery cache, edited from the model `e`
picker. See [Reasoning effort](../reference/effort.md).

Multiple instances of the same template (e.g. two `deepseek` instances with
different keys or endpoints) are ordinary: each is its own `[[providers]]` row
with the same `template_id`, and each owns its own credential keyed by its own
`id`. The template defines the routes once; instances never repeat them.

## Path 2: Built-in provider (per-provider file)

Create a new file `crates/muta-providers/src/registry/<name>.rs`. Each
provider file owns three things: a model-id list, a baseline metadata table,
and a template spec. Use `deepseek.rs` as a minimal reference.

```rust
use muta_contracts::thinking::ThinkingSupport;
use muta_contracts::{Model, WireFormat};

use super::ProviderTemplateSpec;

/// The model ids this provider serves (display order).
pub const ACME_BUILTIN_MODELS: &[&str] = &["acme-1"];

/// Baseline capability metadata — context window, thinking support, effort
/// levels, wire format. Submitted to `muta_contracts`'s registry at link time.
pub const MODELS: &[Model] = &[
    Model {
        id: "acme-1",
        name: "Acme One",
        family: "acme",
        context_window: 128_000,
        thinking: ThinkingSupport::None,
        tool_call: true,
        vision: false,
        format: WireFormat::OpenAi,
        model_guidance: "",
        effort_levels: &[],
    },
];

inventory::submit!(muta_contracts::model::BaselineModels(MODELS));

pub(crate) const TEMPLATE_SPEC: ProviderTemplateSpec = ProviderTemplateSpec {
    id: "acme",
    baselines: MODELS,
    protocol: "openai",
    models: ACME_BUILTIN_MODELS,
    discovery: true,
    fitting: false,
};
```

Then wire the file into the aggregate tables in
`crates/muta-providers/src/registry/mod.rs`:

1. Add `pub mod acme;` (alphabetical order).
2. Add `pub use acme::ACME_BUILTIN_MODELS;` to the re-export block.
3. Add `acme::TEMPLATE_SPEC` to the `PROVIDER_TEMPLATE_SPECS` array.

The catalog loops over `PROVIDER_TEMPLATE_SPECS` automatically, so no `match`
arm is needed. `build_provider_for_channel` constructs the concrete
`OpenAiChatCompletionsProvider`, stamping the template `id` so assistant
messages are attributed correctly. The `MODELS` table feeds the model
registry via `inventory` at link time — `resolve("acme-1")` returns the
context window and capabilities you declared, with no manual registration
call.

### Optional: persist the API key

An instance's credential is stored in `credentials.toml` keyed by instance id
(`[providers.<id>] api_key`), or read live from an `api_key_env` env var when
the instance declares one. No code change is needed — every instance already
resolves its credential this way. The catalog resolves env-first
(`api_key_env`), then `credentials.toml`, then empty (a keyless relay sends no
bearer).

## Path 3: Standalone adapter (incompatible contract)

Use this path only when the provider's contract is genuinely incompatible with
OpenAI Chat Completions. `GoogleProvider` (`Google`) and
`AnthropicMessagesProvider` (`Anthropic`) are the existing examples, exposed
through `muta-llm-client` and re-exported by `muta-providers`.

Implement a `Provider` struct with at minimum `chat` and `stream_chat`. Both
methods receive one `ModelRequest` containing the messages and tool
declarations for that call. Consume both values directly; do not cache request
inputs in the provider.

Decide explicitly whether to implement the optional structured-stream method:

| Method | If implemented | If omitted (trait default) |
|--------|----------------|---------------------------|
| `stream_chat_events` | Provider emits `TextDelta`, `ReasoningDelta`, `ToolCallDelta` | Provider emits only `TextDelta`; reasoning and tool-call deltas are lost |

For native function calling, translate `ModelRequest.tool_specs` into the
provider's tool-declaration shape and translate native tool calls back into
`Message.tool_calls` or `ToolCallDelta`. An adapter without native function
calling may ignore `tool_specs`; it should return `tool_calls: None`. The
agent's compatibility path then parses text-emitted tool calls instead of
native `tool_calls`; provider implementations do not call the fallback
parser directly.

Then wire the adapter into the two construction sites:

1. Add a `Transport` variant in `crates/muta-contracts/src/catalog.rs` and an arm
   in `build_provider_for_channel`
   (`crates/muta-providers/src/registry/mod.rs`) that constructs the adapter
   from the channel.
2. Register the template in `PROVIDER_TEMPLATE_SPECS` (add a `route_for_model`
   arm if the adapter routes by model wire format) so the catalog's derivation
   (`crates/muta-agent/src/catalog/derive.rs`) exposes it by `id`.

Map muta's `Role` enum to the provider's role names in both `chat` and
`stream_chat`. The universal fallback assumes assistant text is reachable
through the standard message channel; a misnamed role breaks it.

## Verify

```bash
cargo test -p muta-providers
cargo test -p muta-agent catalog
```

Then exercise the provider end-to-end:

1. Set the API key env var and start the agent with
   `default_provider = "acme"` in `config.toml`.
2. Send a prompt that should trigger a tool call. Confirm the tool step
   renders with the right arguments and result.
3. If the model advertises reasoning support (e.g. an `acme-reasoner`
   variant), switch to it and confirm a thinking step appears.
4. Switch to the provider from inside the TUI with `/models` and confirm the
   header updates and the new model is used.
5. Repeat the tool-call test on a provider that uses the universal fallback
   (a test adapter that ignores `ModelRequest.tool_specs`) to confirm the new
   provider behaves consistently across both transports.

## Update documentation

- Add a row to the appropriate table in [Providers](../reference/providers.md)
  (registry preset table for OpenAI-compatible presets, bespoke table for
  standalone adapters). User-defined entries need no doc change — they are
  config, not code.
- If the provider introduces a new capability shape (e.g. a third standalone
  adapter), update
  [Provider capabilities](../explanation/provider-capabilities.md).
- If the provider's env vars or `default_provider` key differ from the obvious
  naming, call that out explicitly.

## See also

- [Providers](../reference/providers.md) — existing provider matrix
- [Provider capabilities](../explanation/provider-capabilities.md) — capability
  layering and why providers differ
- [Request flow](../explanation/request-flow.md) — the wire contract registry
  presets inherit
- [ADR-0002](../adr/0002-model-channel-abstraction.md) — the catalog and
  channel abstraction
