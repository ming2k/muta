# ADR-0171: Three-layer model catalog and pluggable network sources

- **Status:** Proposed
- **Date:** 2026-09-02

## Context

A connection's model catalog is assembled from three independent sources —
**user config**, **compiled-in code**, and **live network discovery** — but the
project treats them with inconsistent, partly hard-coded semantics. This
proposal formalizes them as a single three-layer overlay and makes the network
source pluggable. The immediate beneficiary is the `opencode-go` preset, whose
model list and wire-format routing are currently hand-maintained and stale; the
shape generalizes to any preset whose first-party endpoint is not an adequate
model catalog.

Three current pain points:

1. **The `id` set is not an overlay, it is a single-source branch.**
   `route_models` (`crates/muta-agent/src/catalog/derive.rs:68`) resolves to
   exactly one layer per connection: a `preset_id` connection uses the live
   discovery result *or* the compiled snapshot (`spec.models`), never both;
   a custom connection (no `preset_id`) uses only `connection.models`. There
   is no notion of "user list layered over code over network".

2. **Capability fields are already three-layer, but the vocabulary doesn't
   match.** ADR-0149 defines the fixed order
   `user config > remote metadata > local baseline` for per-channel capability
   fields (`ModelCapabilities::for_channel` + `apply_overrides`), and this is
   well-tested. But it is expressed in "capability" terms, not "catalog"
   terms, so the two axes (which models exist, what each model can do) read as
   unrelated mechanisms.

3. **`opencode-go`'s wire routing is a string special-case.**
   `route_for_model` (`crates/muta-providers/src/registry/mod.rs:206`) embeds
   an `if spec.id == "opencode-go"` branch that maps a model's wire protocol
   to four different relay base URLs by hand. The model list itself
   (`OPENCODE_GO_MODELS` / `OPENCODE_GO_SERVED_MODELS` / `MODELS` in
   `registry/opencode_go.rs`) is a 20-entry hard-coded table that has drifted
   from the relay's actual catalog (models.dev lists 33 opencode-go entries,
   including `glm-5.3`, `kimi-k3`, `grok-4.5/4.6`, `qwen3.8-*`, `gpt-5.6-luna`).
   Adding a model or a multi-format relay means editing Rust by hand.

The reference `opencode` project (TypeScript) solves this with a single central
catalog (`https://models.dev/api.json`) fetched once, cached on disk with TTL,
and refreshed on a schedule — providers become data rows keyed by id, and their
wire SDK is a field (`api.npm`) rather than bespoke code.

## Decision

Formalize the three sources as a **single three-layer overlay** and make the
**network source pluggable** per preset. The two axes are made explicit:

- **Axis A — the model `id` set a connection offers** (which models exist).
- **Axis B — per-model capability/wire metadata** (what each model can do).

Both axes resolve with the same fall-through rule already established by
ADR-0149: *a field/`id` resolved by a higher layer wins; an absent one at a
higher layer falls through to the layer below.*

### Layer model (both axes)

```text
Layer 1  user config     RouteSettings::capability_overrides   (per instance+model)
                         + custom connections' declared id list (axis A)
                         + preset-connection id-list extension (axis A — deferred)
Layer 2  network         live catalog (pluggable source)       (per preset connection)
Layer 3  compiled code   baseline tables in registry/<provider>.rs
```

- **Layer 3 (compiled)** remains the offline floor: `baselines` + the preset's
  seed `models`. It is always present for a preset connection (it defines the
  preset's identity — protocol, endpoint, known-good wire formats) and is the
  intersection allowlist for non-fitting discovery. It is never absent for a
  preset; a fully-user connection has no preset and therefore no layer 3.
- **Layer 2 (network)** is optional and now expressed by a pluggable source,
  replacing the `discovery: bool` boolean with a data value (see below).
- **Layer 1 (user)** already exists as `RouteSettings::capability_overrides`
  (axis B). For **axis A (the id set)**, the user layer is currently realized
  only by *pure-custom* connections (`preset_id: None`, whose `connection.models`
  are served verbatim); a *preset* connection cannot extend the id set the
  preset/network layers derive. Extending the user layer to preset connections
  (an explicit user-supplied id list overlaying layer 2/layer 3) is **deferred**:
  the custom-connection path already gives users full control over id sets,
  models.dev auto-discovery reduces the demand, and the feature would carry real
  persistence + reconciliation + UI cost without a concrete requirement (see
  Consequences → Deferred).

### Pluggable network source

Replace `ProviderPresetSpec.discovery: bool` with an optional, data-driven
source declaration:

```rust
pub enum LiveCatalog {
    /// The provider's own model-catalog endpoint. The concrete request shape
    /// is the declared `DiscoveryProtocol` — a provider whose catalog endpoint
    /// deviates from the standard wire-derived shape (ChatGPT's Codex backend,
    /// Google's Antigravity cloudcode surface) declares its own scheme here
    /// rather than being sniffed from auth or URL.
    ProviderEndpoint(DiscoveryProtocol),
    /// A third-party catalog entry (e.g. the `opencode-go` entry on
    /// models.dev), keyed by provider id.
    ModelsDev { provider: &'static str },
    /// The provider's own endpoint first, falling back to a models.dev entry
    /// when the first-party catalog fails. This is the "official upstream
    /// first, third-party catalog as the resilience net" shape (zai).
    ProviderEndpointWithFallback {
        protocol: DiscoveryProtocol,
        fallback_provider: &'static str,
    },
}

// ProviderPresetSpec
pub live_catalog: Option<LiveCatalog>,
```

- `discovery: false` → `live_catalog: None` (compiled list only).
- `discovery: true` → `live_catalog: Some(LiveCatalog::ProviderEndpoint(scheme))`.
- `opencode-go` → `live_catalog: Some(LiveCatalog::ModelsDev { provider: "opencode-go" })`.
- `zai` → `live_catalog: Some(LiveCatalog::ProviderEndpointWithFallback { protocol: OpenAi, fallback_provider: "zai" })` — first-party stays authoritative, models.dev covers the gap on failure.

**The first-party scheme is itself declared, not inferred.** The previous
`DiscoveryProtocol::for_connection` sniffed the endpoint shape from the
connection's auth (ChatGPT → Codex) and `models_endpoint_for` sniffed the
Google Antigravity (cloudcode) surface from the base URL. Both are now folded
into the framework as *declared* `DiscoveryProtocol` values on the preset
(`chatgpt-oauth` declares `Codex`; `antigravity-oauth` declares
`GoogleCloudCode`), so a provider with a bespoke catalog endpoint states its
own scheme in one place instead of being special-cased in the discovery engine.

Both network variants produce the same normalized `Vec<DiscoveredModel>` and
flow through the existing reconciliation pipeline in
`muta-agent/src/catalog/discovery.rs` (baseline intersection / fitting /
`DiscoveryCache` / `register_fitted_models`). The network layer remains
best-effort: on fetch/parse failure it silently retains the last good subset —
never blanking a working connection.

### New `muta-models-dev` crate (standalone, removable source module)

A self-contained crate that owns the third-party catalog:

- fetches `https://models.dev/api.json` with a UA + timeout;
- caches to disk (XDG cache) with a TTL and a cross-process file lock;
- ships a **committed, pruned snapshot** (`crates/muta-models-dev/snapshot.json`)
  for offline fallback — refreshed by
  `scripts/refresh-models-dev-snapshot.sh`, embedded via `include_str!` so
  builds never touch the network and stay deterministic;
- exposes one function: `provider_models(provider_id) -> Result<Vec<DevModel>, Error>`
  (schema-neutral types; the mapping to `DiscoveredModel` lives in
  `muta-providers`, keeping this crate dependency-free of the client);
- parses capabilities (family, context, reasoning, vision, effort tiers from
  `reasoning_options`) into the same `DiscoveredModel` shape as live discovery.

The crate is *removable at the workspace level*: it is an ordinary path
dependency of `muta-providers` / `muta-runtime` (no cargo `[features]`
gate), so removing it means dropping those two dependencies and the handful
of `LiveCatalog::ModelsDev` / fallback call sites — an architectural
separation, not a compile-time switch. A build without it degrades to the
compiled baseline layer, so opencode-go still works offline. Deliberately
**not** cargo-feature-gated: no consumer has asked for a models.dev-free
build, and `#[cfg]`-gating every `ModelsDev` reference across the registry,
routing, reconciler, and presets would tax the whole test/config matrix for a
speculative configuration. It is not a new mechanism — it is an alternative
*source* for the existing Layer-2 network feed.

**Runtime background refresh.** The crate exposes a
[`DynamicModelsDev`](`muta_contracts::DynamicCatalog`) whose `refresh()` keeps
the disk cache fresh on an hourly cadence; the generic `spawn_refresh` wiring
runs it at startup (`muta-runtime::bootstrap`). Discovery reads the refreshed
cache on its own schedule (startup, `/refresh`, per-round ETag), so a
long-running daemon's catalog stays within a couple of hours of upstream.

**CI freshness guard.** `scripts/check-models-dev-snapshot.sh` (wired into CI)
compares the committed snapshot against the live catalog and fails when a
provider the client consumes has models the snapshot lacks, forcing a
`scripts/refresh-models-dev-snapshot.sh` run + commit. A *stale* snapshot is a
hint, not a correctness gate: the live fetch always wins when online.

### Wire-format routing becomes data, not a string special-case

The `if spec.id == "opencode-go"` branch in `route_for_model` is generalized
into a per-preset **wire-override table** (data), so any multi-wire relay is
declared, not coded:

```rust
// provider-preset field; empty for single-protocol presets
pub wire_overrides: &'static [(&'static str, WireProtocol)],
```

`opencode_go.rs` shrinks to this override table (models whose wire format
differs from the relay's default OpenAI chat: `minimax-*` → Anthropic
`/messages`, etc.) plus the `live_catalog` declaration. The bulk baseline
moves to the `muta-models-dev` feed. `route_for_model` resolves a model's wire
protocol by: wire-override table → fitted/remote protocol → registry baseline
(default OpenAI chat), then selects the matching relay URL.

## Alternatives considered

- **Keep hand-maintained `opencode_go.rs` (status quo).** Rejected: the
  table has demonstrably drifted, every model addition is a Rust edit +
  recompile, and the `route_for_model` string special-case does not scale to
  more multi-format relays.
- **Per-provider `Discovery` trait / interface.** Rejected: muta deliberately
  models OpenAI-compatible providers as *data* to avoid ~30 lines of delegating
  boilerplate per provider (see `OpenAiProviderSpec` doc). The first-party
  discovery surface collapses to a few protocol *shapes* (`DiscoveryProtocol`:
  OpenAi/Anthropic/Google/Codex), not one per provider. A trait buys nothing
  here and adds the boilerplate we removed.
- **`discovery: bool` + a separate `models-dev` flag.** Rejected: two booleans
  encode a 3-state decision (none / first-party / third-party) as an
  inconsistent pair. A single `Option<LiveCatalog>` makes "no network" the
  absence of a value, not a fourth enum arm.
- **Treat `models.dev` as a separate "third mechanism".** Rejected on the
  user's observation that both first-party `/models` and models.dev are
  network requests — the real axis is *who serves the data*, and both must
  feed the same Layer-2 pipeline.

## Consequences

- **Positive:** new models on opencode-go appear after a catalog refresh, no
  Rust edit; the three-layer rule is uniform across the id set and capability
  fields; wire routing is data; the models.dev client is a standalone,
  workspace-removable source module.
- **Negative:** adds a network dependency (a path dependency, not a feature
  flag) and a new crate; the embedded snapshot must be refreshed periodically
  to stay useful offline.
- **Neutral:** ADR-0149's capability order is unchanged and reinforced; custom
  (non-preset) connections are unaffected.
- **Deferred:** preset-connection user id-list extension (axis A user layer) is
  not implemented. The user layer for id sets exists today only via pure-custom
  connections. Revisit if a concrete need emerges for adding arbitrary model ids
  to a preset connection (e.g. a user whose account unlocks a relay model the
  catalog does not list); the custom-connection path is the current workaround.
- **Migration (four steps, each shippable alone):**
  1. Extract `LiveCatalog` enum + `LiveCatalog::ProviderEndpoint(DiscoveryProtocol)`,
     replacing `discovery: bool`, and fold the first-party special cases
     (Codex, GoogleCloudCode) into *declared* schemes — pure refactor, no
     behavior change. *(Done: `discovery` → `live_catalog` across all 11
     presets; `for_connection`/URL-sniffing removed; `DiscoveryProtocol`
     gained `GoogleCloudCode`.)*
  2. Add the `muta-models-dev` crate (cache/TTL/fetch/offline snapshot). *(Done:
     the crate fetches `models.dev/api.json`, caches under the XDG cache with a
     TTL + cross-process lock, returns schema-neutral `DevModel`s, and embeds a
     **committed pruned snapshot** refreshed by
     `scripts/refresh-models-dev-snapshot.sh`; `muta-providers` maps the types
     to `DiscoveredModel`.)*
  3. Switch `opencode-go` to `live_catalog: Some(LiveCatalog::ModelsDev{..})`
     + the wire-override table. *(Done: the stale `OPENCODE_GO_SERVED_MODELS`
     allowlist is deleted; `fitting: true` materializes catalog-only models
     (`glm-5.3`, `kimi-k3`, …) with zero client changes; a small
     `WIRE_OVERRIDES` table pins the `minimax-*` family to Anthropic
     `/messages` so fitted models route correctly; `route_for_model` consults
     it before the registry.)*
  4. Extend the pluggable network source to a **first-party-with-fallback**
     shape (`LiveCatalog::ProviderEndpointWithFallback`), demonstrated on `zai`
     (ids-only first-party, models.dev as the resilience net); add the runtime
     hourly cache refresh (`DynamicModelsDev` via `spawn_refresh`) and the CI
     snapshot freshness guard. *(Done.)*

## References

- ADR-0149 — three-layer model capability resolution order (axis B precedent).
- ADR-0161 — route-scoped inference protocols (`WireProtocol`).
- `opencode` reference implementation: `ModelsDev` service
  (`packages/core/src/models-dev.ts`), `fromModelsDevProvider`
  (`packages/opencode/src/provider/provider.ts`).
- models.dev API: `https://models.dev/api.json`.
