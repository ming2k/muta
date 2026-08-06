# How to add a provider

This guide walks through wiring a new LLM provider into neenee. For the
existing provider matrix, see [Providers](../reference/providers.md). For the
capability model that decides which path to take, see
[Provider capabilities](../explanation/provider-capabilities.md).

neenee resolves every provider through one catalog
(`build_catalog` in `crates/neenee-agent/src/catalog.rs`): it materializes
registry presets, bespoke built-ins, and user-defined entries into channels
with fully resolved credentials, then constructs the concrete `Provider` via
`build_provider_for_channel` in
`crates/neenee-providers/src/registry/mod.rs`.
Startup and a `/models` pick share this single path — there is no separate
dispatch `match` to edit for presets or user entries.

## Choose a path

| Provider speaks... | Path | Effort |
|--------------------|------|--------|
| OpenAI Chat Completions, or any endpoint reachable with a URL + key | User-defined entry in `config.toml` | None (no code) |
| OpenAI Chat Completions, and you want it shipped as a built-in | Per-provider file in `registry/` | Small |
| A genuinely incompatible contract (different roles, no `tools` field) | Standalone adapter | Medium |

Prefer the config path for private or self-hosted endpoints, and the registry
path for a vendor preset every neenee user would want.

## Path 1: User-defined entry (no code)

Any OpenAI-compatible, Google-native, or Llama endpoint can be added from
`config.toml` without touching code. Add a `[[providers]]` table whose `id`
either overrides a built-in or introduces a new model:

```toml
default_provider = "acme"

[[providers]]
id = "acme"
name = "Acme"

[[providers.channels]]
label = "default"
transport = "OpenAi"          # or "Anthropic" or "Google"
base_url = "https://api.acme.example/v1/chat/completions"
api_key_env = "ACME_API_KEY"        # env var wins over the inline key below
model = "acme-1"
```

A **native-Google relay / 中转站** uses `Google`. The `base_url` is the
versioned base (carry the `/v1beta` prefix — the `/models/{id}:generateContent`
path is appended for you). Auth stays on the `?key=` query param:

```toml
default_provider = "my-gemini-relay"

[[providers]]
id = "my-gemini-relay"
name = "My Google Relay"

[[providers.channels]]
label = "default"
transport = "Google"
base_url = "https://relay.example.com/v1beta"
api_key_env = "GEMINI_RELAY_KEY"
model = "gemini-2.5-flash"
```

To redirect the **built-in** `google` preset instead (so picking `google` in
`/models` and `default_provider = "google"` route through the relay), set the
top-level `google_base_url` (or export `GOOGLE_BASE_URL`):

```toml
default_provider = "google"
google_base_url = "https://relay.example.com/v1beta"
```

The legacy spellings `gemini_base_url` / `GEMINI_BASE_URL` are still accepted
as aliases.

Per-channel fields:

| Field | Meaning |
|-------|---------|
| `transport` | `OpenAi`, `Anthropic`, or `Google` |
| `base_url` | Full chat-completions URL (OpenAI), `/messages` URL (Anthropic), or **versioned Google base** (native Google, e.g. `https://relay.example.com/v1beta` — the `/models/{id}:generateContent` path is appended for you) |
| `api_key_env` | Env var name read first; empty values fall through |
| `api_key` | Inline key, used when `api_key_env` is unset or empty |
| `model` | Wire model id; falls back to the entry `id` when omitted |
| `user_agent` | OpenAI-compatible and native Google |
| `effort` | Optional reasoning depth for OpenAI or Anthropic reasoning models; clamped to the model's supported levels |
| `thinking` | Optional Anthropic thinking on/off switch; ignored by OpenAI and Google |

An entry whose `id` matches a built-in replaces it entirely; a new `id` is
appended. One entry may carry several `channels` (e.g. a model reachable
through several relays), with `default_channel` selecting the active one. See
[ADR-0002](../adr/0002-model-channel-abstraction.md) for the channel model.

## Path 2: Built-in provider (per-provider file)

Create a new file `crates/neenee-providers/src/registry/<name>.rs`. Each
provider file owns three things: a model-id list, a baseline metadata table,
and a template spec. Use `deepseek.rs` as a minimal reference.

```rust
use neenee_core::thinking::ThinkingSupport;
use neenee_core::{Model, WireFormat};

use super::ProviderTemplateSpec;

/// The model ids this provider serves (display order).
pub const ACME_BUILTIN_MODELS: &[&str] = &["acme-1"];

/// Baseline capability metadata — context window, thinking support, effort
/// levels, wire format. Submitted to `neenee_core`'s registry at link time.
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

inventory::submit!(neenee_core::model::BaselineModels(MODELS));

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
`crates/neenee-providers/src/registry/mod.rs`:

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

### Optional: persist the API key in config

By default a built-in resolves its API key from `config.toml`/`credentials.toml`
through the catalog, not from an environment variable. To let users persist it
in `credentials.toml` (or read it from an `api_key_env` channel field), add the
provider id to `CREDENTIALED_BUILTINS` and a corresponding `*_api_key` field on
`Config` in `crates/neenee-persistence/src/config.rs`. The catalog's credential
resolution then picks the config field up after `credentials.toml`, so a preset
works through either path.

## Path 3: Standalone adapter (incompatible contract)

Use this path only when the provider's contract is genuinely incompatible with
OpenAI Chat Completions. `GoogleProvider` (`Google`) and
`AnthropicMessagesProvider` (`Anthropic`) are the existing examples, exposed
through `neenee-llm-client` and re-exported by `neenee-providers`.

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

1. Add a `Transport` variant in `crates/neenee-core/src/catalog.rs` and an arm
   in `build_provider_for_channel`
   (`crates/neenee-providers/src/registry/mod.rs`) that constructs the adapter
   from the channel.
2. Materialize the entry in `build_catalog`
   (`crates/neenee-agent/src/catalog.rs`) so the catalog exposes it by `id`.

Map neenee's `Role` enum to the provider's role names in both `chat` and
`stream_chat`. The universal fallback assumes assistant text is reachable
through the standard message channel; a misnamed role breaks it.

## Verify

```bash
cargo test -p neenee-providers
cargo test -p neenee-agent catalog
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
