# 0258. First-Class Declarative Model Providers and Connection Pipe Purity

- **Status:** Accepted
- **Date:** 2026-10-15
- **Implementation:** `muta-contracts`, `muta-persistence`, `muta-providers`, `muta-agent`, `muta-runtime`, `mutx`
- **Builds on:** ADR-0123 (provider instances are state; routes are derived), ADR-0171 (three-layer model catalog), ADR-0199 (unified cascading model resolution), ADR-0201 (model provider service surface and connection identity)
- **Supersedes:** ADR-0201 §6 (the `custom` pseudo-provider on Connection instances)

## Context and Problem Statement

ADR-0201 defined the core invariant:
$$\text{ModelProvider} \equiv (\text{endpoint family, wire dialect, model universe})$$
$$\text{Connection} \equiv \text{a named credentialed pipe}$$

However, ADR-0201 left one significant pragmatic compromise in place: **Section 6, The generic provider `custom`**.
To allow users to bring custom OpenAI-compatible relays without authoring code, ADR-0201 allowed `Connection` instances to carry `base_url`, `protocol`, and `user_agent` when pointing to `provider = "custom"`.

This compromise created acute architectural debt:
1. **Conceptual Smear on Connection**: `Connection` was supposed to be a pure credentialed pipe, yet it owned physical transport endpoints and protocol settings.
2. **Duplication on Multi-Credential Custom Relays**: If a team had multiple keys or accounts on the same corporate proxy/relay, every `Connection` entry had to duplicate `base_url`, `protocol`, and `user_agent`.
3. **Asymmetric Provider Kinds**: Built-in providers had their specs defined in code (`ModelProviderSpec`), while custom providers were pseudo-specs embedded within `Connection` structs.
4. **Discovery Routing Disconnect**: Because `Connection` owned the endpoint while `ModelProviderSpec` owned catalog discovery, discovery for `custom` connections was hardcoded to `DiscoveryProtocol::OpenAi`, ignoring the connection's wire protocol (e.g. breaking custom Anthropic/Google relays).

## Decision Drivers

- **Zero Legacy Baggage**: Eliminate the `custom` pseudo-provider compromise completely.
- **Single Vocabulary and Domain Model**: Built-in and user-declared providers must share identical domain properties (`root_url`, `default_protocol`, `catalog`, `dialect`).
- **Connection Purity**: A `Connection` must strictly bind a credential (`auth`, `api_key_env`) and client identity to a `provider_id`, plus optional model narrowing/aliasing (`models`). It must not own any network transport properties.
- **First-Class Reusability**: A user-defined provider is declared once and can be referenced by multiple `Connection` pipes.

## Decision Outcome

We adopt **First-Class Declarative Model Providers and Connection Pipe Purity**.

### 1. The Declarative Provider Store (`model_providers.toml`)

`model_providers.toml` (XDG config) is promoted from just holding model scoping rules to holding full user-declared provider definitions alongside model scoping:

```toml
# User-declared custom providers (First-Class Service Surfaces)
[providers.corp-relay]
label = "Corporate Relay"
root_url = "https://relay.corp.example/v1"
default_protocol = "chat-completions" # chat-completions | responses | anthropic-messages | google-gemini
catalog = { source = "endpoint", format = "openai" } # or format = "anthropic" | "google" | "none"
dialect = "deepseek"                  # standard | deepseek | openrouter | copilot
client_profile = "cursor"             # native | opencode | claude-code | codex | cursor | windsurf | etc. (ADR-0164)

[providers.local-vllm]
label = "Local vLLM Server"
root_url = "http://localhost:8000/v1"
default_protocol = "chat-completions"
catalog = { source = "endpoint", format = "openai" }

# Scoped model rules remain keyed by provider id (per ADR-0199/ADR-0201)
[model_providers.corp-relay]
models.include = [{ id = "glm-5.2", context_window = 128000 }]
```

### 2. Connection Pipe Purity (`connections.toml`)

`Connection` loses `base_url`, `protocol`, `user_agent`, and `catalog_source`. It becomes a pure credential pipe:

```toml
[[connections]]
name = "work-dev"
provider = "corp-relay"       # References the user-declared provider in model_providers.toml
api_key_env = "CORP_DEV_KEY"
models.include = ["glm-5.2"]

[[connections]]
name = "work-prod"
provider = "corp-relay"       # References the same provider with a different credential pipe
api_key_env = "CORP_PROD_KEY"

[[connections]]
name = "deepseek-direct"
provider = "deepseek"         # References a built-in provider
```

### 3. Unified In-Memory Provider Lookup

The runtime provider registry merges built-in specs (`crates/muta-providers/src/registry/mod.rs`) and user-declared specs from `model_providers.toml` into a single provider lookup table:

$$\text{effective\_spec}(p) = \text{UserDeclaredProvider}(p) \;\;{\lor}\;\; \text{BuiltinProvider}(p)$$

No code in `muta-agent`, `muta-runtime`, or `mutx` checks whether a provider is "custom" vs "built-in". All providers are equal `ModelProviderSpec` values.

### 4. Migration and Clean Break

When loading existing `connections.toml`:
1. If a connection has `provider = "custom"`, a user-declared provider named `custom-<connection-name>` is synthesized in memory with the connection's `base_url`, `protocol`, and `user_agent`, and persisted to `model_providers.toml`.
2. The connection's `provider` is updated to `custom-<connection-name>`, and the legacy fields on `Connection` are dropped upon write.
3. No deprecated fields remain in the serialized schema.

## Invariants

- **`[INV-PROV-01]` Connection Pipe Purity**: `Connection` must never declare network protocol, base URL, or catalog discovery endpoints.
- **`[INV-PROV-02]` Homogeneous Provider Model**: Any capability available to built-in providers (catalog discovery, wire protocol, dialect, prompt caching policy) must be representable in declarative user providers.
- **`[INV-PROV-03]` Id Uniqueness**: Custom provider IDs must not collide with built-in provider IDs. Collisions fail validation at configuration load time.

## Consequences

### Positive
- Strict separation of concerns: Service Surface vs Credential Pipe.
- Multi-account / multi-key setups on internal relays require defining the endpoint only once.
- Eliminates special-casing of `custom-openai` or `provider = "custom"` across the codebase.

### Negative
- Configuration schema change requiring automatic load-time migration of existing `connections.toml` files.

## References
- ADR-0123: Provider instances are state; routes are derived
- ADR-0199: Unified cascading model resolution architecture
- ADR-0201: ModelProvider is the service surface, Connection is a named pipe
