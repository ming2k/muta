# 0065. Runtime-fitted model capability overlay

- **Status:** Accepted
- **Date:** 2026-07-16

## Context

Model metadata (context window, reasoning, vision, wire format, effort tiers)
lives in the static `KNOWN_MODELS` registry
(`crates/platform/neenee-core/src/model.rs`), and every provider feature flows
from `model::resolve()`. Each upstream model release therefore required a
client change and a release: a new registry entry, a new template snapshot
entry, and updated tests — even though the Kimi Code platform's live
`GET /models` already advertises everything the registry encodes
(`context_length`, `supports_reasoning`, `supports_thinking_type`,
`supports_image_in`, `think_efforts`). Kimi K3's release (2026-07) made the
cost concrete: the platform began serving `k3` with a 1M context window, and
the client only picked it up through a manual registry bump.

At the same time, live model-list discovery
(`discover_provider_models` in `crates/platform/neenee-agent/src/catalog.rs`)
deliberately intersects the advertised ids with the static registry. That
intersection is what keeps an arbitrary relay from fabricating channels, but
it also discards the rich capability fields trusted official endpoints
publish, and it drops platform-native ids the registry has not learned yet
(the Kimi platform lists `kimi-for-coding` / `kimi-for-coding-highspeed`,
while the registry historically tracked the `kimi-k2.7-code` alias).

## Decision

Introduce a **runtime-fitted capability overlay** with a strict precedence
order and an explicit trust gate:

1. **Static registry first.** `model::resolve(id)` returns the vetted
   `KNOWN_MODELS` entry whenever one exists. A provider can never override,
   extend, or downgrade a known model.
2. **Fitted overlay for unknown ids.** `model::register_fitted_models`
   (`neenee-core`) accepts `FittedModel` values for ids the registry does not
   know and stores them in a process-wide overlay consulted by `resolve()`
   before the conservative fallback. `Model` stays `Copy` over `&'static str`;
   fitted strings/slices are interned via `Box::leak` (bounded by the number
   of provider-advertised ids).
3. **Trust is a per-template decision.** `ProviderTemplateSpec` gains a
   `fitting` flag, enabled only for official first-party endpoints whose
   `/models` advertises real capability fields — today `kimi-code` (which
   publishes `think_efforts.valid_efforts`) and `copilot-oauth` (which
   publishes `supports.reasoning_effort`). Relays and sub2api templates keep
   the historical registry-intersection behavior: their advertised ids are
   never trusted with capability metadata.
4. **Persist what was fitted.** Discovery stores the fitted subset as
   `UserProviderConfig.fitted_models` (ids absent from the registry only), so
   the overlay repopulates at startup from disk (`sync_fitted_model_registry`)
   and works offline; a failed refresh leaves the last good values in place.
   Reconciliation treats fitted ids as retainable alongside registry ids.
5. **Discovery parses capability hints generically.** `list_models` returns
   `DiscoveredModel` entries whose capability fields are `None` for the stock
   OpenAI/Anthropic/Gemini shapes; the Kimi platform fields
   (`context_length`, `supports_thinking_type` — which takes precedence over
   the legacy `supports_reasoning` bool — `supports_image_in`,
   `think_efforts.valid_efforts`, `display_name`) are read when present.
6. **Legacy instances follow the template.** A `kimi-code` instance stamped
   `ModelSource::Fixed` before the template supported discovery upgrades to
   `Api` at reconcile time; that `Fixed` was assigned by the backfill at a
   moment the template offered no `Api` source, so it cannot have been a
   deliberate opt-out.

## Alternatives considered

- **Keep bumping the static registry per release.** Rejected: every upstream
  release costs a client change and a release cycle, and the information is
  already published upstream in machine-readable form. (The registry remains
  the vetted base layer, not a growth path.)
- **Trust every provider's advertised metadata.** Rejected: a malicious or
  sloppy relay could inflate a model's context window or claim vision support
  the model lacks, turning a conservative failure (unknown model → safe
  fallback) into request-time blowups. Trust is therefore a template-level
  allowlist, not a global switch.
- **Thread a catalog reference through every `resolve()` caller instead of a
  process-wide overlay.** Rejected as disproportionate: `resolve()` has many
  call sites (pressure budget, vision gating, display names, effort clamping)
  and a read-mostly `RwLock` overlay populated at startup keeps the change
  additive. The leaked-interning cost is bounded and one-time per id.
- **Fit from models.dev instead of provider APIs.** Rejected for this use
  case: it adds an external dependency with a different freshness/consistency
  model, while the authoritative source (the provider that will actually
  serve the model) already publishes the same fields. models.dev remains the
  curation source for the static opencode-go catalogue.

## Consequences

- New Kimi platform models become usable with zero client changes: channels
  appear via live discovery, and context window / reasoning / vision / effort
  resolve through the same `model::resolve` every consumer already uses.
- The static registry stays the single source of truth for every id it knows;
  fitting only ever fills the unknown-id gap, and the conservative fallback
  still covers everything else.
- `kimi-code` and `copilot-oauth` instances change behavior at startup: a
  background `GET /models` refresh (with the snapshot as fallback) and, for
  `kimi-code`, a one-time `Fixed → Api` source upgrade, persisted on first
  reconcile.
- `kimi-code` instances now expose the platform-native ids
  (`kimi-for-coding`, `kimi-for-coding-highspeed`) as first-class channels;
  the `kimi-k2.7-code` alias remains registered (it is also the opencode-go
  wire id) and keeps working.
- Trust misuse is a config/code-review concern, not a runtime risk: enabling
  `fitting` on a template requires editing the compiled-in template table.

## References

- `neenee_core::model::{FittedModel, register_fitted_models, resolve}`
  (overlay and precedence)
- `neenee_providers::list_models::DiscoveredModel` (capability parsing)
- `neenee_store::config::{UserProviderConfig::fitted_models, FittedModelInfo}`
  (persistence)
- `neenee_agent::catalog::{discover_provider_models,
  reconcile_provider_models, sync_fitted_model_registry}` (wiring)
- ADR-0046 (reasoning is opt-in per model), ADR-0048 (session as single
  source of truth)
- External prior art: kimi-code CLI's managed provider consumes the same
  `/models` capability fields at runtime
  (`packages/oauth/src/managed-kimi-code.ts`).
