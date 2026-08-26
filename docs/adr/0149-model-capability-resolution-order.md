# 0149. Three-layer model capability resolution order

- **Status:** Accepted (initially mis-numbered as ADR-0080 at adoption; renumbered to ADR-0149 on 2026-09-09 to clear the collision with the neenee → `neenee-cli` rename, which owns 0080)
- **Date:** 2026-08-26

## Context

The client's model capability metadata (context window, vision, thinking
style, tool-call support, effort ladder, wire format) already flows through
several sources, but their precedence was documented only in fragments:

- The static **baseline registry** — per-provider `MODELS` tables in
  `muta-providers`, linked in via `inventory::submit!(BaselineModels)`.
  The single source of capability truth for ids-only endpoints
  (Zhipu's coding `/models` returns only `{id, object, created, owned_by}` —
  verified live 2026-08).
- **Remote model metadata** (`RemoteModelMetadata`) — what a trusted
  endpoint advertised at discovery time, overlaid field-wise onto the
  baseline by `ModelCapabilities::for_channel` (ADR-0070).
- **User route settings** (`RouteSettings`, keyed
  `providers[<instance_id>][<model_id>]` in state) — which until now could
  only steer effort/thinking, never correct a *capability* the other two
  layers got wrong.

Three concrete gaps motivated formalizing the order:

1. `glm-5.3-flash` shipped on Zhipu's coding plan with no client model-list
   entry — the offering snapshot (`ZAI_CODE_MODELS`) had drifted from the
   platform with no guard.
2. A relay can serve an id with different capabilities than the official
   endpoint (`glm-5.2` via opencode-go vs. Z.AI), and the user had no way to
   correct, say, a wrongly-advertised `vision: true` for *their* account.
3. `for_channel` merged two layers while the third (user) was applied at a
   different site with different plumbing — the order was emergent, not
   designed.

## Decision

The effective capabilities for a channel resolve in a **fixed three-layer
order; a field resolved by a higher layer wins, and an absent field at a
higher layer falls through to the layer below**:

```text
1. user config        RouteSettings::capability_overrides (per instance+model)
2. remote metadata    RemoteModelMetadata (fitting:true templates only)
3. local baseline     the static registry entry for the model id
```

Layer responsibilities:

- **Layer 3 (baseline)** owns everything the endpoint does not say. It is
  maintained by hand in the provider registry files; the offering list and
  the baseline table are guarded against drift by
  `template_models_are_covered_by_the_local_baseline_table`, and shared ids
  across provider tables are guarded byte-identical by
  `shared_baseline_ids_are_identical_across_provider_tables`.
- **Layer 2 (remote)** may only *overlay explicitly advertised fields*
  (ADR-0070 semantics, unchanged). Ids-only endpoints (zai-code) contribute
  visibility only — `fitting: false`.
- **Layer 1 (user)** is a new `CapabilityOverrides` record
  (`family`/`context_window`/`max_output_tokens`/`thinking`/`tool_call`/
  `vision`, all `Option`, `None` = no opinion). `Some(false)` is meaningful:
  it forces a capability off even when both lower layers claim it. It lives
  in `muta-contracts` beside the merge function; persistence stores it keyed
  per `(instance_id, model_id)` inside `RouteSettings` (state, never cache —
  the user's corrections must survive "reset caches").

Mechanically:

- `ModelCapabilities::for_channel` stays a pure baseline⊕remote merge.
- `ModelCapabilities::apply_overrides(&CapabilityOverrides)` stamps layer 1
  and is the *single* auditable place a user wins over a provider.
- `Channel` carries `user_overrides: Option<CapabilityOverrides>`;
  `Channel::capabilities()` applies it after the remote overlay, so every
  transport builder (Google/OpenAI/Anthropic/custom) inherits the order from
  one call site.
- `derive_channel` wires `route_settings.capability_overrides` into the
  channel, filtering empty records so no-op overrides are never persisted.

## Consequences

- The precedence is now enforced at the type level: a user override, a
  remote advertisement, and a registry baseline cannot be applied in any
  other order without going around `Channel::capabilities()`.
- New-model onboarding has a fixed checklist
  (`docs/dev/new-model-onboarding.md`); capability numbers must cite their
  source in the baseline entry comment.
- A future TUI editor for capability overrides needs no new resolution
  logic — it only writes `RouteSettings::capability_overrides`.
- Shared-id baselines remain the fragile seam: the consistency test guards
  present duplicates, but a *new* provider table re-deriving a shared id
  still fails only at test time, not at authoring time.
