# 0203. Remote Catalog Overlay and Connection-Gated Model Pipeline

- Status: Accepted
- Date: 2026-09-09
- Scope: providers, catalog, persistence, contracts, runtime, tui
- Deciders: Muta maintainers
- Consulted: -
- Informed: -
- Builds on: ADR-0123 (provider instances are state; routes are derived), ADR-0149 (capability resolution order), ADR-0171 (three-layer model catalog), ADR-0199 (unified cascading model resolution), ADR-0201 (ModelProvider service surface and Connection named pipe)
- Supersedes: ADR-0065 (runtime fitted capability overlay), the `fitting: bool` spec field, and the vocabulary of "discovery"

---

## Context and Problem Statement

Model availability, capability resolution, and admission have evolved through multiple transitional layers:
1. **The `fitting: bool` abstraction (ADR-0065)** was an overloaded pseudo-concept. It conflated two orthogonal decisions: *Model Admission* (whether an unlisted remote ID should materialize as a usable channel) and *Capability Trust* (whether remote capability advertisements should be believed).
2. **The "Discovery" terminology was semantically vacuous**. In distributed systems, discovery refers to service/peer registration. In this system, the operation was solely querying an upstream model listing, leading to conflation between agent discovery, tool discovery, and catalog querying.
3. **OpenAI and generic relay endpoints exposed bare IDs with zero capability metadata**. While specialized coding platforms (ChatGPT Codex, Kimi Coding, GitHub Copilot) return rich context windows and reasoning tiers, standard endpoints (`GET /v1/models`) return bare strings mixed with non-chat artifacts (`text-embedding-3`, `tts-1`, `babbage-002`). Under the legacy `fitting: false` regime, these endpoints were clamped to subtractive intersections (`Baseline ∩ Discovery`), making it impossible to use newly deployed frontier models (such as `gpt-6-astra`) without an upstream client binary update.
4. **Dual-track fallback heuristics (ADR-0171) broke determinism**. Attempting to query an endpoint first and fall back to third-party catalogs (`models.dev`) under flakiness caused state-flapping: models and capability flags oscillated between consecutive synchronizations.
5. **Connection lifecycle ambiguity**. Following ADR-0201's clean break establishing that `ModelProvider` is the service surface and `Connection` is a named pipe, the system lacked a mathematically closed specification for how upstream provider catalogs flow through the connection's pipe filter.

We enact an uncompromising, clean-break architectural overhaul: eliminating `fitting`, retiring `discovery`, enforcing pluggable single-source remote catalogs, standardizing field-level tristate sparse overlays, and establishing the Connection as an isolated algebraic pipe.

---

## Decision Drivers

- **Long-Termism & Clean Break**: Zero transitional aliases, zero legacy shims. Retire all historical patch-work (`fitting: bool`, dual-track fallback heuristics, subtractive-only hard-clamps).
- **Domain Precision**: Replace `discovery` with `RemoteCatalog`. Every entity must name its exact functional reality.
- **Strict Determinism**: Single-source ingestion per provider/connection. The catalog state for a given source revision must be a pure, reproducible function.
- **Sovereign User Control**: The user's connection pipe possesses absolute authority over model admission and exclusion, completely unconstrained by vendor omissions or upstream delays.
- **Zero-Guessing Safety**: No regex or prefix-based heuristic hallucination of model capabilities. Unknown models degrade to safe conservative plain-text baselines unless augmented by verified catalogs or explicit user declarations.
- **Offline & Flakiness Resilience**: Read-through cache with atomic ETag background revalidation. Network failures must never destroy local operational state.

---

## Considered Options

- **Option 1: Retain `fitting: bool` and append heuristic archetypes**. Keep `fitting` and add pattern-matching rules (e.g. `gpt-*` implies 128k context + tool calls).
  * *Rejected*: Brittle and dangerous. Prefix heuristics break silently when providers release non-chat variants (e.g., realtime, audio, or reasoning-incompatible snapshots).
- **Option 2: Dynamic multi-source waterfall fallback**. Query the provider endpoint, fall back to `models.dev`, fall back to OpenRouter, fall back to compiled baseline.
  * *Rejected*: Introduces Heisenbugs. Non-deterministic network timing alters model capabilities and token boundaries between runs.
- **Option 3 (Chosen): Pluggable Single-Source Remote Catalog Overlay with Connection-Gated Pipeline**.
  1. A `ModelProvider` ingests from exactly one pluggable remote source.
  2. The upstream catalog is derived via **Tristate Sparse Merge**: `Baseline ⊕ RemoteCatalog`.
  3. The `Connection` acts as an independent pipe governed by a deterministic valve equation: `Output = (Filter(Candidates) ∪ Inject) \ Block`.
  4. Connection instantiation snapshots the preset's policy rules into `connections.toml` (sandbox isolation), while upstream catalog additions continue to flow dynamically through the pipe's filter.

---

## Decision Outcome

We adopt **Option 3**. The architecture is partitioned into two cleanly decoupled domains:
1. **The Upstream Provider Supply Domain** (`ModelProvider` Master Catalog);
2. **The Downstream Connection Pipeline Domain** (`Connection` Named Pipe Valve).

```text
 ┌─────────────────────────────────────────────────────────────┐
 │                  Provider Master Catalog                    │
 │  Level 0: Builtin Baseline Spec                             │
 │       │                                                     │
 │       ▼ ⊕ Tristate Sparse Overlay (Single Remote Source)    │
 │  Level 1: RemoteCatalog (Endpoint OR ModelsDev)             │
 └──────────────────────────────┬──────────────────────────────┘
                                │ Candidates Pool C
                                ▼
 ┌─────────────────────────────────────────────────────────────┐
 │              Connection Named Pipe (connections.toml)       │
 │                                                             │
 │  1. Policy Gate: Filter(C, policy)                          │
 │       policy ∈ { BaselineOnly, OpenAll, Glob(pattern) }     │
 │                                                             │
 │  2. Sovereign Injections: ∪ Inject                          │
 │       Bypasses filter unconditionally; carries metadata     │
 │                                                             │
 │  3. Absolute Interceptions: ∖ Block                         │
 │       Removes models unconditionally                        │
 └──────────────────────────────┬──────────────────────────────┘
                                │ Effective Channels
                                ▼
 ┌─────────────────────────────────────────────────────────────┐
 │        Deterministic 4-Tier Presentation Sort               │
 │  Favorites (*) ──► Active ──► Curated Baseline ──► Dynamic  │
 └─────────────────────────────────────────────────────────────┘
```

---

### 1. Domain Entities & Vocabulary

The system unifies on the ontology ratified in ADR-0201:

| Concept | Retired Vocabulary | Canonical Form | Responsibility |
| :--- | :--- | :--- | :--- |
| Model list sync | `discover_provider_models`, `live_catalog` | **`RemoteCatalog` / `sync_remote_catalog`** | Querying external network sources for model sets and capability facts. |
| Ingestion trust | `fitting: bool` | **`RemoteCatalogSource` + `TristateMerge`** | Specifying the authoritative remote source and sparse field merging rules. |
| Upstream service | `preset_id`, `ProviderPresetSpec` | **`ModelProvider` / `ModelProviderSpec`** | The protocol, baseline spec, and remote catalog driver for a vendor surface. |
| Connection pipe | `Connection.extra_models`, `preset` | **`Connection`** | The named pipe carrying credentials, client profile, and admission valve rules. |
| Global user rules | `presets.toml` | **`model_providers.toml`** | User-defined model scopes and capability overrides keyed by `provider_id`. |

---

### 2. Single Pluggable Remote Catalog Source

A `ModelProviderSpec` binds to exactly one `RemoteCatalogSource`. Dual-track fallbacks are strictly prohibited:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteCatalogSource {
    /// Query the provider's first-party endpoint (e.g. OpenAI /v1/models, ChatGPT Codex backend).
    Endpoint(EndpointProtocol),
    /// Query a verified third-party structured catalog (e.g. models.dev entry).
    ModelsDev { provider_id: &'static str },
    /// No remote network sync; compiled baseline is authoritative.
    None,
}
```

#### Provider Default Bindings:
- **`openai` (API Platform)**: Binds to `ModelsDev { provider_id: "openai" }`. This solves the decade-old bare-ID problem: OpenAI's endpoint omits capabilities, so muta uses `models.dev` as its authoritative remote catalog source, dynamically acquiring context windows, vision support, and reasoning tiers for newly released models without binary updates.
- **`openai-subscription` (ChatGPT Codex)**: Binds to `Endpoint(EndpointProtocol::Codex)`. The subscription backend already advertises rich capability metadata (`context_window`, `supported_reasoning_levels`, `visibility`).
- **`github-copilot`**: Binds to `Endpoint(EndpointProtocol::Copilot)`.
- **`kimi-code`**: Binds to `Endpoint(EndpointProtocol::OpenAiCompatible)`.
- **`custom`**: Binds to `Endpoint(EndpointProtocol::OpenAiCompatible)` by default, overridable per connection.

#### Connection Source Override:
A connection targeting an unverified private proxy or relay may cleanly redirect its catalog source to `models.dev` without altering its transport endpoint:
```toml
[[connections]]
name = "corporate-relay"
provider = "openai"
base_url = "https://internal-relay.corp/v1"
catalog_source = { models_dev = "openai" }
```

---

### 3. Provider Master Catalog: Tristate Sparse Overlay

When remote catalog data arrives, it is overlaid onto the compiled `BaselineModels` to produce the `ProviderMasterCatalog`:

$$\text{MasterCatalog}(p) = \text{Baseline}(p) \oplus \text{RemoteCatalog}(p)$$

The overlay operator $\oplus$ is evaluated per model ID using **Tristate Sparse Merging**:
- **`Absent`**: The remote record did not mention this capability field. The baseline value is preserved intact.
- **`Present(val)`**: The remote record explicitly specified this field. It overrides the baseline unconditionally, even when specifying `false`, empty arrays, or `0`.

```rust
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelCapabilityPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<ReasoningSupport>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vision: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort_levels: Option<Vec<String>>,
}
```

---

### 4. Zero-Heuristic Degradation for Unannotated Bare IDs

If an upstream endpoint yields a model ID that exists neither in the compiled baseline nor in an annotating catalog:
1. **No Regex Guessing**: The system must never inspect model substrings (`gpt-*`, `claude-*`) to guess context lengths or reasoning flags.
2. **Safe Degradation**: If admitted by the connection pipe, the model materializes with **Safe Conservative Text Defaults**:
   - `context_window`: 128,000 tokens (safe standard floor).
   - `tool_call`: `false` (prevents invalid schema errors).
   - `vision`: `false` (prevents 400 payload rejections).
   - `thinking`: `ReasoningSupport::None` (prevents unsupported parameter injections).
3. **User Elevation**: To unlock full capabilities for an unannotated model, the user explicitly supplies the verified fields via `inject` or `overrides`.

---

### 5. Connection Named Pipe: Valve Algebra & Rule Snapshotting

#### Creation-Time Rule Snapshotting (Sandbox Isolation):
When a connection is instantiated from a provider template:
- The template's **policy rules** (`filter`, `inject`, `block`) are written into `connections.toml`.
- Materialized static model ID strings are **never** written.
- Existing connections are immune to future template changes in `model_providers.toml`. However, upstream catalog additions from the provider dynamically pass through the connection's live filter rule.

#### Connection Pipe Valve Algebra:
For candidate models $C$ emitted by the Provider Master Catalog:

$$\text{EffectiveChannels}(c) = \Big( \text{Filter}(C, c.\text{filter}) \cup c.\text{inject} \Big) \setminus c.\text{block}$$

Where:
- **`Filter(C, policy)`** applies the primary admission rule:
  - `FilterPolicy::BaselineOnly`: Admits only models present in the compiled baseline (default for strict relays).
  - `FilterPolicy::All`: Admits every model delivered by the remote catalog (default for official providers).
  - `FilterPolicy::Glob(patterns)`: Admits models matching explicit globs (e.g. `["gpt-5*", "gpt-6*"]`).
- **`c.inject` (Sovereign Injection)**: Forced inclusion. Injected models bypass `filter` unconditionally. If an injected model defines capability facts, they populate the model's metadata.
- **`c.block` (Absolute Interception)**: Forced exclusion. Matching IDs are pruned unconditionally.

#### Configuration Schema (`connections.toml`):
```toml
[[connections]]
name = "openai-main"
provider = "openai"

# Valve Policy
filter = "all"

# Sovereign Injections (supports inline capability declarations)
inject = [
  { id = "gpt-6-astra", context_window = 1_050_000, vision = true, tool_call = true }
]

# Absolute Interceptions
block = ["gpt-4o-mini", "text-embedding-*"]

# Optional Client Emulation Override (None inherits provider.default_client_profile)
# client_profile = "cursor"
```

---

### 6. Cascading Capability Resolution

For any effective channel model $m$, its runtime capabilities are resolved via a deterministic 4-layer right-biased cascade:

$$\text{EffectiveCapabilities}(m) = \text{ConnectionOverrides}(c, m) \prec \text{ProviderOverrides}(p, m) \prec \text{MasterCatalog}(p, m) \prec \text{ConservativeFloor}$$

Where $\prec$ denotes sparse field-level inheritance: a non-`None` field in a higher layer overrides lower layers; `None` falls through.

---

### 7. Dual-Layer Client Profile Emulation Contract

Upstream endpoints frequently inspect HTTP `User-Agent` and companion headers for anti-bot WAF mitigation, feature gating, or rate-limit tiers (e.g. ChatGPT Codex requires `originator: codex_cli_rs` and matching UA; GitHub Copilot requires editor plugin headers; Google Antigravity requires `x-goog-api-client`).

Client emulation is modeled as a two-tier **Factory Baseline vs. Pipeline Override** contract:

1. **Provider Spec declares recommended baseline and sensitivity**:
   ```rust
   pub struct ModelProviderSpec {
       // ...
       /// Factory recommended/required client profile for WAF and quota compliance.
       pub default_client_profile: ClientProfile,
       /// When true, deviating from this profile risks 403 WAF rejection.
       pub client_profile_sensitive: bool,
   }
   ```
2. **Connection retains sovereign pipeline override authority**:
   ```rust
   pub struct Connection {
       pub name: String,
       pub provider: String,
       /// Sparse override: None inherits Provider.default_client_profile.
       pub client_profile: Option<ClientProfile>,
       // ...
   }
   ```
3. **Runtime Transport Assembly**:
   $$\text{EffectiveClientProfile} = \text{Connection}.\text{client\_profile} \lor \text{Provider}.\text{default\_client\_profile} \lor \text{ClientProfile::Native}$$

This guarantees zero-configuration safety for novice users (never triggering WAF blocks on sensitive endpoints like Codex), while granting corporate and advanced users total freedom to inject custom enterprise proxy headers.

---

### 8. Offline Resilience & Cache Contract

1. **Local Read-Through**: All UI surfaces and session initializers read directly from `$XDG_CACHE_HOME/muta/catalog_cache/<connection_name>.json`. Startup is strictly 0ms synchronous I/O.
2. **Background ETag Revalidation**: Network synchronization occurs asynchronously on a background worker with HTTP `If-None-Match`:
   - `304 Not Modified`: TTL renewed; zero memory/disk mutation.
   - Network failure / HTTP 429 / 5xx: The local cache remains completely untouched. A transient notification (`CatalogSyncWarning`) is emitted. Existing channels **never** disappear due to transient network issues.
   - `200 OK`: Evaluates $\text{Baseline} \oplus \text{RemoteCatalog}$, writes atomically to disk, and signals the UI to refresh.

---

### 9. Staged Rollout Error Translation

When using third-party catalogs (such as `models.dev`), a model may appear in the catalog before a specific user's API key has received rollout access from the upstream vendor.

The runtime protocol driver intercepts upstream errors during prompt execution:
- HTTP `404 model_not_found` or HTTP `403 permission_denied` for an advertised model is automatically translated into an actionable diagnostic notice:
  > *"Model `gpt-6-astra` is registered in the upstream catalog but returned 404/403. Your API key or organization has not yet been granted rollout entitlement by OpenAI."*
- This eliminates ambiguous "API error" reports and accurately reflects the vendor's staged rollout reality.

---

### 10. Presentation Ergonomics & UI Keymap Mapping

TUI user interactions map 1:1 directly to the pipe's algebraic state mutations:
- **Pressing `a` (Add / Inject)**: Appends the entered model ID to `connection.inject`.
- **Pressing `x` (Hide / Block)**: Appends the selected model ID to `connection.block`.
- **Pressing `e` (Edit Overrides)**: Writes the updated effort, vision, or context overrides directly to `connection.overrides.<model_id>`.

Presentation sort order is strictly deterministic and computed via a 4-tier tuple key:

```rust
models.sort_by_key(|m| {
    (
        !m.is_favorite,          // Tier 1: User-starred (*) models pinned to top
        !m.is_active_selection,  // Tier 2: Active session model placed next
        m.curated_order_index,   // Tier 3: Curated baseline sequence (flagship -> balanced -> fast)
        &m.id                    // Tier 4: Discovered remote additions sorted stably by ID
    )
});
```

---

## Invariants & Behavioral Boundaries

- **`[INV-CATALOG-01]` Zero Regex Heuristics**: Runtime capability values must never be synthesized via regex pattern matching on model identifiers.
- **`[INV-CATALOG-02]` Single-Source Ingestion**: A provider or connection remote catalog sync must query exactly one configured `RemoteCatalogSource`. Automatic multi-source fallback waterfalls are strictly prohibited.
- **`[INV-CATALOG-03]` Non-Destructive Cache**: Network timeouts, DNS failures, or HTTP 4xx/5xx responses during catalog sync must never evict or diminish the existing local catalog cache.
- **`[INV-CATALOG-04]` Sovereign Injections**: Models defined in `connection.inject` must bypass `connection.filter` unconditionally.
- **`[INV-CATALOG-05]` Rule-Only Persistence**: Connection instantiation must persist filter policy rules (`filter = "baseline"` | `"all"` | globs), never frozen lists of materialized model ID strings.
- **`[INV-CATALOG-06]` Tristate Preservation**: Deserialization of remote capability patches must distinguish between missing fields (`None`) and explicitly falsified fields (`Some(false)`).
- **`[INV-CATALOG-07]` Client Profile Inheritance**: A connection with an omitted `client_profile` must inherit the provider's `default_client_profile`. Providers with `client_profile_sensitive: true` must declare their companion headers statically within that profile.

---

## Positive Consequences

- **Immediate Frontier Adoption**: Newly released models (such as `gpt-6-astra`) are usable immediately upon release via `models.dev` or `inject`, requiring zero client binary patches.
- **Total Operational Isolation**: Individual connections function as independent sandboxes; modifying one connection or global provider defaults cannot silently mutate existing configured pipes.
- **Zero Configuration Drift**: The removal of legacy shims and the unification of terminology eliminates developer cognitive overhead across all crates.
- **Resilient Offline Operation**: Full agentic capability remains 100% operational in air-gapped or flaky network environments using cached and baseline models.

## Negative Consequences & Mitigations

- **Catalog Sync Lag**: If an upstream provider releases a model and `models.dev` has not yet indexed it, a bare-ID endpoint will only deliver a safe text fallback.
  * *Mitigation*: The user can press `a` in the TUI to inject the model with known context and capability parameters immediately.
- **Staged Rollout Entitlement Disconnect**: Models indexed globally may be selected by users whose individual accounts lack access.
  * *Mitigation*: The runtime error translation layer detects `model_not_found` and emits clear staged-rollout explanations.

---

## Rejected Alternatives & Negative Knowledge

### Regex-Based Model Archetype Inference
- *Why considered*: To avoid user data entry when unannotated `gpt-6-*` models appear.
- *Why rejected*: Unacceptable failure modes. Prefix heuristics break silently when vendors repurpose naming conventions for embedding, audio, or reasoning-incompatible models. Explicit declarations and catalog augmentation are the only robust paths.

### Multi-Source Waterfall Fallbacks
- *Why considered*: To provide redundancy when an endpoint fails.
- *Why rejected*: Non-deterministic flapping. Flaky connections caused models to oscillate between endpoint definitions and third-party definitions across consecutive refreshes. Single-source determinism is non-negotiable.

### Materializing Static ID Arrays on Connection Creation
- *Why considered*: To achieve complete snapshot isolation.
- *Why rejected*: Produces "zombie connections" that permanently miss upstream provider baseline improvements and security updates. Persisting the *filter rule* achieves connection isolation while maintaining dynamic catalog flow.

---

## Implementation Plan

1. **Contracts & Persistence (`muta-contracts`, `muta-persistence`)**:
   - Retire `fitting: bool` on `ModelProviderSpec`.
   - Implement `RemoteCatalogSource`, `ModelCapabilityPatch`, and `ConnectionFilterPolicy`.
   - Update `connections.toml` serialization to store `filter`, `inject`, and `block`.
2. **Catalog & Providers (`muta-providers`, `muta-agent`)**:
   - Bind `openai` provider to `RemoteCatalogSource::ModelsDev`.
   - Implement tristate sparse merge for `ProviderMasterCatalog`.
   - Implement connection valve algebra pipeline in `catalog::derive`.
3. **Runtime & TUI (`muta-runtime`, `mutx`)**:
   - Implement staged rollout error translation in response handlers.
   - Wire TUI keybindings (`a`, `x`, `e`) directly to pipe mutations.
   - Implement 4-tier deterministic presentation sorting in `/models`.
