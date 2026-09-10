# 0201. ModelProvider is the service surface, Connection is a named pipe

- **Status:** Accepted
- **Date:** 2026-09-09
- **Implementation:** Landed in one release across `muta-contracts`, `muta-providers`, `muta-persistence`, `muta-agent`, `muta-runtime`, `mutx`, and the web mirror. The wire protocol version moved to 6 (`PROTOCOL_VERSION` and `MIN_PROTOCOL_VERSION`) per ADR-0134; `connections.toml` migrates on load via a read-only `RawConnection` shape that is never serialized back, so no alias survives in the schema. Provider id vocabulary lives in `muta_contracts::model_providers::MODEL_PROVIDER_IDS` (it is persisted schema), and a registry test asserts the `muta-providers` table covers it exactly.
- **Builds on:** ADR-0123 (provider instances are state; routes are derived), ADR-0149 (capability resolution order), ADR-0164 (first-class client profiles and connection emulation), ADR-0171 (three-layer model catalog), ADR-0199 (unified cascading model resolution)
- **Supersedes:** the `preset_id` join key, `presets.toml`, the wire vocabulary `AddProvider` / `EditProvider` / `ConnectProvider`, and `ModelTargetScope::Preset`

## Context and Problem Statement

Provider connectivity in this repository is expressed with three words that each
carry two meanings. The result is a vocabulary that the code, the configuration
files, and the user-facing surfaces each resolve differently.

### 1. `provider` names two opposite things

| Meaning | Where it appears today |
| :--- | :--- |
| **Upstream service** (the thing that serves models) | the `muta_contracts::Provider` trait; `ProviderPresetSpec` (`crates/muta-providers/src/registry/mod.rs:142`); `LiveCatalog::ModelsDev { provider }` (`crates/muta-providers/src/registry/mod.rs:68`) |
| **The connection itself** | `AgentRequest::AddProvider` (`crates/muta-contracts/src/events.rs:154`), `EditProvider` (`crates/muta-contracts/src/events.rs:188`); `crates/muta-runtime/src/handlers_provider.rs`; `apps/tui/crates/mutx/src/providers.rs` |

The user interface already works around this: `provider_type_label(preset_id)`
(`apps/tui/crates/mutx/src/providers.rs:333`) exists solely to recover "which
upstream service" from a field named `preset_id`. That accessor is evidence that
`preset_id` is, in fact, the provider identity.

### 2. `preset_id` serves two lifecycles at once

- **Runtime identity.** `ProviderPresetSpec::id` documents itself as "the durable
  join key between a connection and its preset"
  (`crates/muta-providers/src/registry/mod.rs:143`).
- **Creation-time template.** `ProviderPreset`
  (`apps/tui/crates/mutx/src/providers.rs:47`) is the option table for the
  add-connection chooser.

One field, two roles. The runtime role must be persisted; the template role must
not be.

### 3. `preset_id: Option<String>` creates two kinds of connection

- `Connection::is_preset()` (`crates/muta-persistence/src/connections.rs:133`).
- `protocol`, `base_url`, and `user_agent` are meaningful only when
  `preset_id = None` (`crates/muta-persistence/src/connections.rs:58`).
- The TUI special-cases the `custom-openai` identifier
  (`apps/tui/crates/mutx/src/event_loop/actions/modals.rs:102`).
- An unknown preset id silently degrades into the pure-custom kind
  (`crates/muta-runtime/src/handlers_provider.rs:209`), so a typo changes a
  connection's kind instead of failing.

ADR-0199 unified the model scopes, but the two-kind split remains.

### 4. Model scopes are keyed by a concept the file does not name

`ModelScopeConfig` is stored in two places whose keys read alike but mean
different things:

- `connections.toml` — keyed by connection instance.
- `presets.toml` (`crates/muta-persistence/src/presets.rs:17`) — keyed by
  `preset_id`, which is the provider identity.

A user reading `presets.toml` cannot tell that the settings are per provider.

### 5. Cost of doing nothing

Every new provider, every new surface, and every AI-assisted change must first
decide which of the two `provider` meanings is in play. The two-kind connection
split keeps producing special cases (asymmetric handlers before ADR-0199, the
`custom-openai` branch today). The configuration keeps inviting users to attach
model settings to the wrong scope.

## Decision Drivers

- **One word, one meaning.** `ModelProvider`, `Connection`, `preset` (template),
  and `Channel` must each name exactly one concept.
- **The model provider is the emphasized structure.** Model existence and
  capability facts belong to the upstream service, not to a credential binding.
- **A connection is a pipe.** It binds a provider to a credential and a client
  identity, and may narrow the provider's model set. It does not define models.
- **Clean break.** ADR-0199 set the precedent of retiring a field with a load-time
  migration and no transitional aliases. This decision follows it.
- **No silent divergence.** Invalid provider references and duplicate connection
  names must fail loudly rather than degrade.

## Considered Options

- **Option 1: Rename and re-scope (chosen).** `preset_id` becomes `provider_id`;
  the provider registry entry becomes `ModelProviderSpec`; the connection drops
  its id in favor of a unique `name`; model scopes are keyed by provider id;
  `preset` survives only as a creation-time template.
- **Option 2: Rename only.** Rename the types and fields without changing the
  connection shape or the scope keying.
- **Option 3: Document the vocabulary only.** Add a glossary entry and leave the
  code as is.
- **Option 4: Retire the connection.** Model only providers and accounts.
- **Option 5: Protocol-named generic providers.** A family such as
  `openai-compatible` / `anthropic-compatible` / `google-compatible`.

## Decision Outcome

Chosen option: **Option 1**, because Options 2 and 3 leave the ambiguity in the
code, and Options 4 and 5 remove or duplicate concepts the system needs.

### 1. One vocabulary

| Concept | Current spelling | Adopted spelling |
| :--- | :--- | :--- |
| Upstream LLM service | `preset_id`, `ProviderPresetSpec`, "provider type" | **`ModelProvider`**, identifier `provider_id`, spec `ModelProviderSpec` |
| Creation-time template | `ProviderPreset` | `ConnectionTemplate` (user-facing copy may still say "preset") |
| Credentialed pipe instance | `Connection`, but called "provider" on the wire and in the UI | **`Connection`**; wire requests become `AddConnection` / `EditConnection` / `ConnectConnection` |
| Bring-your-own endpoint | `preset_id = None` (a connection kind) | `provider = "custom"` (an ordinary `ModelProvider` entry) |
| Per-model derived transport | `Channel` | unchanged |

### 2. ModelProvider identity criterion

A `ModelProvider` identifies a real service surface:

```text
ModelProvider ≡ (endpoint family, wire dialect, model universe)
```

Two access modes are the same `ModelProvider` if and only if all three are equal
and only the authentication method differs.

Consequences of the criterion:

| Axis | May appear in a `provider_id`? | Why |
| :--- | :--- | :--- |
| Wire protocol | **No** | It is orthogonal: any endpoint can speak any of the four canonical protocols (`WireProtocol` is a closed set). Encoding it duplicates a field. |
| Authentication mode | **No** — it belongs on `Connection.auth` | One surface can support several. `crates/muta-providers/src/registry/xai.rs:1` documents "SuperGrok OAuth **or** `XAI_API_KEY`" for a single endpoint. |
| Region / subscription tier | **Yes** | It correlates with the endpoint: an API key cannot reach the ChatGPT backend, and the CN and international GLM endpoints are different hosts. |

### 3. Connection identity: `name` is the primary key

`Connection` loses its `id` field. The instance `name` is the unique identifier,
and duplicate names are rejected with a suggested alternative instead of being
silently disambiguated.

Today `Connection::id` is the join key for six stores:

| Store | Reference | Effect of an unrewritten rename |
| :--- | :--- | :--- |
| `credentials.toml` | `[connections.<name>]` | API key lost (hard) |
| `auth.toml` | `[tokens.<name>]` | OAuth tokens lost; re-login required (hard) |
| `config.toml` | `default_connection` | default silently falls back |
| discovery cache | `model_lists[connection_id]` | cache miss; re-fetched |
| `connection_usage` | recency and `last_models` | recency and last-model memory lost |
| sessions and telemetry | attribution | historical attribution splits |

Rename is therefore an **atomic transaction** that rewrites the two hard
dependencies (`credentials.toml`, `auth.toml`) and `default_connection`. The
regenerable stores are allowed to expire, and historical records are **not**
rewritten retroactively: a session keeps the connection name it ran under. The
wire gains an explicit `RenameConnection { from, to }` request so the transaction
has one owner.

Hand-editing `name` in `connections.toml` is equivalent to replacing the
connection: the credential becomes an orphan. The loader detects orphaned
credential entries and warns; `muta auth set <name>` repairs the pairing.

### 4. Provider-keyed model scopes

Model scopes are keyed by `provider_id` and stored in
`model_providers.toml` (renamed from `presets.toml`):

```toml
[model_providers.deepseek]
models.include = [{ id = "deepseek-v4-preview", context_window = 1_000_000 }]
models.exclude = ["deepseek-chat-deprecated"]

[model_providers.deepseek.models.overrides."deepseek-v4-pro"]
max_output = 32_768
```

A connection may narrow or override, and may not invent:

```text
effective_models(c) = (Baseline(p) ∪ Include(p) ∪ Include(c)) ∖ Exclude(p) ∖ Exclude(c)
capabilities(m)     = Overrides(c, m) ≺ Overrides(p, m) ≺ DiscoveryMetadata(c, m) ≺ Baseline(p, m)
```

The `custom` provider is exempt from the "may not invent" rule because its model
universe is open by definition.

### 5. Provider identifier renaming

The UI labels already name the service surfaces correctly; only the identifiers
lag. The `-oauth` suffix encodes an authentication mode and is removed.

| Label | Current id | Adopted `provider_id` |
| :--- | :--- | :--- |
| OpenAI Platform | `openai` | `openai` |
| ChatGPT Subscription | `chatgpt-oauth` | `openai-subscription` |
| Anthropic | `anthropic` | `anthropic` |
| Google AI Studio | `google` | `google` |
| Google Antigravity | `antigravity-oauth` | `google-antigravity` |
| GitHub Copilot | `copilot-oauth` | `github-copilot` |
| xAI | `xai-oauth` | `xai` |
| DeepSeek | `deepseek` | `deepseek` |
| ZAI Code (CN) | `zai-code` | `glm-cn` |
| Kimi Code | `kimi-code` | `kimi-code` |
| OpenCode Go | `opencode-go` | `opencode-go` |
| Custom connection | `custom-openai` | `custom` |

The `-code` suffix stays: a coding-plan endpoint is a distinct service surface
(different host, different model universe), not a distinct authentication mode.
`zai-code` is renamed because its endpoint is `open.bigmodel.cn` (the CN
platform), not the international `z.ai` brand.

### 6. The generic provider `custom`

`custom` is an ordinary `ModelProvider` entry with an open model universe, no
compiled baseline requirement, and a default protocol of
`chat-completions` that a connection may override:

```toml
[[connections]]
name = "acme-relay"
provider = "custom"
protocol = "chat-completions"
base_url = "https://relay.example.com/v1"
models.include = ["acme-7b", "acme-13b"]
```

This absorbs the former pure-custom kind. `Connection.protocol`, `base_url`, and
`user_agent` become uniform optional overrides instead of custom-only fields.

### 7. Migration

| Surface | From | To |
| :--- | :--- | :--- |
| Persisted field | `Connection.preset_id` | `Connection.provider` |
| Persisted file | `presets.toml [presets.<id>]` | `model_providers.toml [model_providers.<id>]` |
| Wire requests | `AddProvider` / `EditProvider` / `ConnectProvider` | `AddConnection` / `EditConnection` / `ConnectConnection` (+ new `RenameConnection`) |
| Wire scope | `ModelTargetScope::Preset(String)` | `ModelTargetScope::Provider(String)` |
| Types | `ProviderPresetSpec` | `ModelProviderSpec` |
| TUI types | `ProviderPreset` | `ConnectionTemplate` |
| Provider ids | see §5 | see §5, via a static alias table applied on load |

Both the persistence schema and the wire protocol change, and both change in one
release: the loader accepts the old field names and rewrites them, and the writer
emits only the new names. No transitional aliases survive in serialization.

### Invariants & Behavioral Boundaries

- **INV-1 (Provider identity).** A `provider_id` names a service surface, never a
  wire protocol and never an authentication mode. No new `*-compatible` or
  `*-oauth` provider id may be introduced.
- **INV-2 (Mandatory provider).** Every `Connection` declares exactly one
  `provider_id`, and it must resolve to a registered `ModelProvider`. An unknown
  or blank value is rejected at load — the entry is dropped with an error log and
  is never reinterpreted as a different connection kind — and rejected again at
  request time.
- **INV-3 (Connection identity).** `Connection.name` is the sole identifier.
  `Connection` has no `id` field. Names are unique and compared
  case-insensitively; a duplicate is rejected with a suggested alternative, never
  silently suffixed.
- **INV-4 (Rename atomicity).** Renaming a connection rewrites
  `credentials.toml`, `auth.toml`, and `default_connection` in one transaction or
  not at all. Historical session and telemetry records are never rewritten.
- **INV-5 (Scope ownership).** Model existence and capability facts belong to the
  `ModelProvider`. A `Connection` may narrow the resolved set and override known
  capability fields; it must not declare a model the provider's universe excludes,
  except under `provider = "custom"`.
- **INV-6 (Templates are not identity).** A `ConnectionTemplate` is consumed at
  creation time and never persisted as identity. At most a `created_from`
  provenance field may record the template used.
- **INV-7 (Derived routes).** Channels and routes remain derived at runtime from
  the provider plus the discovery cache and are never persisted (ADR-0123,
  ADR-0182). This decision changes naming and keying, not derivation.

### Positive Consequences

- **One meaning per word.** `provider` no longer means both the upstream service
  and the connection, so code, config, and prose agree.
- **The provider is the emphasized structure.** Model scopes live where the model
  semantics live, and `model_providers.toml` says so in its name.
- **The two-kind connection split disappears.** `is_preset()`, the custom-only
  field trio, the `custom-openai` branch, and the unknown-preset degradation all
  go away.
- **Renaming a connection is safe** for every program-driven path, and the two
  hard dependencies have a single owning transaction.
- **Provider ids are self-describing.** `openai-subscription` and `glm-cn` state
  the service surface; `xai` no longer claims to be OAuth-only.

### Negative Consequences & Trade-offs

- **Breaking persistence and wire change in one release.** Mitigation: a load-time
  migration rewrites old field names and old provider ids; the writer emits only
  new names, following ADR-0199.
- **Hand-editing `connections.toml` becomes less forgiving.** Changing `name` by
  hand orphans the credential. Mitigation: an orphan-credential warning at load
  and `muta auth set <name>` to repair.
- **`name` must be unique case-insensitively**, which constrains a user's naming
  freedom. Mitigation: the rejection carries a suggested alternative.
- **Provider id churn** touches roughly 50 references across code and docs.
  Mitigation: one static alias table in the migration, plus a single sweep.
- **`custom` carries a default protocol** even though its model universe is open.
  This is a prefill for convenience, not an authority; a connection override wins.

## Rejected Alternatives & Negative Knowledge

### Option 2: Rename only (Rejected)

- **Why considered:** lowest cost; removes the `provider` ambiguity without
  touching the connection shape.
- **Why rejected:** leaves `preset_id: Option<String>` and therefore the
  two-kind connection split, the custom-only field trio, and the silent
  unknown-preset degradation. The largest source of special cases survives.

### Option 3: Document the vocabulary only (Rejected)

- **Why considered:** zero code risk.
- **Why rejected:** the code keeps using `provider` with two meanings, so the
  ambiguity re-enters through every new file. Documentation cannot hold a
  boundary the type system does not.

### Option 4: Retire the connection (Rejected)

- **Why considered:** the smallest conceptual surface — providers plus accounts.
- **Why rejected:** discards multiple credentials per provider and per-connection
  client identity, which is the entire value established by ADR-0164.

### Option 5: Protocol-named generic providers (Rejected)

- **Why considered:** an apparent symmetry — one generic entry per wire protocol.
- **Why rejected:** it puts the protocol axis in two places
  (`provider = "anthropic-compatible"` alongside
  `protocol = "chat-completions"`), producing contradictions the type
  system cannot prevent, and it makes `provider` mean "vendor" for curated
  entries and "protocol" for generic ones — the exact overload this ADR removes.
  "Compatible" is relay marketing vocabulary and carries no information once
  `WireProtocol` exists as a closed enum.

### Internal opaque connection id (Rejected)

- **Why considered:** makes rename a pure metadata edit with no transaction, and
  protects hand-edited configurations.
- **Why rejected:** it adds a permanent unreadable field whose only purpose is to
  protect a hand-edit path that already does not carry the credential (the
  credential lives in a separate file behind a separate command). The user-facing
  model — the name is the identity — is simpler and matches the existing
  creation flow.

### Silent slug disambiguation (Rejected)

- **Why considered:** current behavior (`Connections::unique_id`,
  `crates/muta-persistence/src/connections.rs:235`) always succeeds.
- **Why rejected:** silently creating `my-relay-2` when the user asked for
  `my-relay` hides an error the user can fix.

## Links

- Related ADRs: [ADR-0123](0123-provider-instances-as-state-derived-routes.md),
  [ADR-0149](0149-model-capability-resolution-order.md),
  [ADR-0164](0164-first-class-client-profiles-and-connection-emulation.md),
  [ADR-0171](0171-three-layer-model-catalog-and-pluggable-network-sources.md),
  [ADR-0199](0199-unified-cascading-model-resolution-architecture.md)
- Reference documentation to update: [Configuration](../reference/configuration.md),
  [Providers](../reference/providers.md), [Glossary](../reference/glossary.md)
- Task documentation to update: [Add a provider](../how-to/add-a-provider.md)
- Explanation to update: [Provider strategy architecture](../explanation/provider-strategy-architecture.md)
