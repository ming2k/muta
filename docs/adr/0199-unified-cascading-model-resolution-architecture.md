# 0199. Unified Cascading Model Resolution Architecture: Two-Tier Scoped Delta Sets and Field-Level Capability Overlay

- **Status:** Accepted
- **Date:** 2026-09-09
- **Builds on:** ADR-0123 (provider instances are state; routes are derived), ADR-0149 (capability resolution order), ADR-0171 (three-layer model catalog)
- **Supersedes:** ADR-0198 (Declared Models on Preset Connections: per-instance user-intent model lists)

## Context and Problem Statement

Model availability and capability management in the system evolved through several localized decisions, resulting in a fragmented and asymmetrical architecture:

1. **Existence vs. Capability Bifurcation**:
   - Model presence in the picker/catalog followed subtractive discovery (`LiveCatalog::derive` + `supported_model_intersection`), with an ad-hoc union append bolted onto connection instances by ADR-0198 (`Connection.extra_models`).
   - Model capabilities followed a separate three-layer pipeline under ADR-0149 (`capability_overrides` → `RemoteModelMetadata` → baseline).
   - There was no unified concept of a "model definition" encompassing both existence and capability declarations.

2. **Absence of Preset-Level User Customization**:
   - ADR-0198 placed `extra_models` exclusively on `Connection` instances (`connections.toml`).
   - When a user has multiple instances for a preset (e.g., `deepseek-personal`, `deepseek-work`), declaring an unlisted preview model required duplicate declarations across every instance. There was no user-configurable preset scope.

3. **Inability to Exclude/Hide Models**:
   - Neither the preset baseline nor discovery provided a clean way to suppress deprecated or undesired models without deleting discovery altogether. The system only supported monotonic additions, never delta exclusions.

4. **Protocol and Handler Asymmetry**:
   - Runtime requests were split into `RemoveProviderModel` (only valid for pure-custom connections), `AddConnectionModel` (only for preset connections), and `RemoveConnectionModel` (only for preset connection extras).
   - There was no orthogonal mutation contract representing "include model", "exclude model", or "override capabilities" across scopes.

This ADR replaces the piecemeal mechanisms with a single, mathematically rigorous, clean-break **Cascading Model Resolution Architecture**.

## Decision Drivers

- **Long-termism and Clean Break**: Eliminate historical patch layers (`Connection.extra_models`, separate pure-custom model APIs, asymmetrical handlers).
- **Two Scopes (Preset vs. Instance)**: Allow declarative customization globally per provider preset (applying to all instances of that preset) and locally per connection instance.
- **Two Orthogonal Axes (Membership vs. Capabilities)**:
  - **Membership (Existence)** modeled via Set Delta Algebra: Baseline + Discovery + Includes − Excludes.
  - **Capabilities (Metadata)** modeled via Cascading Field-Level Deep Merge: Instance → Preset → Discovery → Baseline.
- **Zero Legacy Baggage**: Cleanly retire `extra_models` in serialization and protocol without transitional aliases.

## Decision Outcome

We adopt the **Unified Cascading Model Resolution Architecture** across contracts, persistence, catalog derivation, and runtime handlers.

### 1. Two-Tier Scoped Model Configuration (`ModelScopeConfig`)

Both the **Preset Scope** (`presets.toml`) and the **Connection Instance Scope** (`connections.toml`) share an identical, orthogonal specification structure:

```rust
pub struct ModelScopeConfig {
    /// Explicitly included/declared models (with optional initial capability facts).
    pub include: Vec<DeclaredModel>,
    /// Explicitly excluded/hidden model ids.
    pub exclude: Vec<String>,
    /// Per-model capability overrides.
    pub overrides: BTreeMap<String, CapabilityOverrides>,
}
```

#### Storage Locations:
- **Preset User Configuration (`presets.toml`)**:
  Located at `~/.config/muta/presets.toml` (XDG config directory).
  Keyed by `preset_id` (e.g., `[presets.deepseek]`, `[presets.openai]`).
- **Connection Instance Configuration (`connections.toml`)**:
  The `Connection` struct drops `extra_models: Vec<DeclaredModel>` and unifies `models: Vec<String>` into:
  ```rust
  pub struct Connection {
      pub id: String,
      pub name: Option<String>,
      pub preset_id: Option<String>,
      // ... auth, protocol, base_url, client_identity ...
      #[serde(default)]
      pub models: ModelScopeConfig,
  }
  ```
  *(For pure-custom connections where `preset_id` is `None`, `models.include` constitutes the full explicit declaration).*

### 2. Set Delta Algebra for Model Membership

For any connection instance $c$ with optional preset $p = c.\text{preset\_id}$:

1. **Base Candidate Set**:
   - If $p$ is present:
     $$S_{\text{base}} = \begin{cases}
       \text{Baseline}(p) \cap \text{Discovery}(c), & \text{if discovery is active and populated} \\
       \text{Baseline}(p), & \text{otherwise}
     \end{cases}$$
   - If $p$ is `None` (pure-custom connection):
     $$S_{\text{base}} = \emptyset$$

2. **Preset-Level Delta Application**:
   $$S_{\text{preset}} = (S_{\text{base}} \cup \text{Includes}(p)) \setminus \text{Excludes}(p)$$

3. **Instance-Level Delta Application**:
   $$S_{\text{effective}} = (S_{\text{preset}} \cup \text{Includes}(c)) \setminus \text{Excludes}(c)$$

Deduplication preserves declaration order: base candidates maintain catalog order, followed by preset additions, followed by instance additions. Exclusions remove targets strictly by exact case-sensitive matching.

### 3. Cascading Field-Level Capability Resolution

For any effective channel model $m \in S_{\text{effective}}$, its capabilities are resolved through a 4-layer descending cascade:

$$\text{EffectiveCapabilities}(m) = \text{InstanceOverrides}(c, m) \prec \text{PresetOverrides}(p, m) \prec \text{DiscoveryMetadata}(c, m) \prec \text{BaselineSpec}(m)$$

Where $\prec$ denotes field-level right-biased fallback: any explicitly specified field in the higher layer takes precedence; `None` falls through to the next layer below.

### 4. Unified Protocol and Runtime Mutation Requests

The divergent requests `AddConnectionModel`, `RemoveConnectionModel`, and `RemoveProviderModel` are superseded by unified, symmetrical agent requests:

```rust
pub enum ModelTargetScope {
    Preset(String),
    Connection(String),
}

pub enum AgentRequest {
    // ...
    /// Set or update model inclusion (with optional initial declared capabilities).
    IncludeModel {
        scope: ModelTargetScope,
        model: DeclaredModel,
    },
    /// Exclude/hide a model from the resolved set.
    ExcludeModel {
        scope: ModelTargetScope,
        model_id: String,
    },
    /// Reset or remove an explicit inclusion or exclusion rule for a model.
    ClearModelRule {
        scope: ModelTargetScope,
        model_id: String,
    },
    /// Set capability overrides for a specific model within a scope.
    SetModelCapabilities {
        scope: ModelTargetScope,
        model_id: String,
        overrides: CapabilityOverrides,
    },
}
```

## Positive Consequences

- **Mathematical Cleanliness**: Model membership is computed via explicit set algebra; capabilities are resolved via a deterministic 4-layer cascade.
- **DRY Across Instances**: Users can define preview models or hide deprecated models once in `presets.toml`, immediately benefiting all current and future connections of that preset.
- **Full Control over Hiding/Disabling**: Users can now exclude unwanted models from the picker without breaking live discovery or patching compiled tables.
- **Unified Protocol**: Frontends interact with one coherent API parameterized by `ModelTargetScope`, eliminating special-case branching for pure-custom vs. preset connections.

## Negative Consequences & Mitigations

- **Breaking Persistence Schema Change**:
  - `Connection.extra_models` is dropped. Existing TOML files using `[[connections.extra_models]]` or bare `models = [...]` are cleanly migrated upon load into `[connections.models]`.
- **Breaking Protocol Change**:
  - Web client wire bindings and TUI request dispatches are updated to the unified request schema.

## References

- ADR-0123 — Provider instances are state; routes are derived, never persisted.
- ADR-0149 — Capability resolution order: user > remote > baseline.
- ADR-0171 — Three-layer model catalog and pluggable network sources.
- ADR-0198 — Declared models on preset connections (superseded by this ADR).
