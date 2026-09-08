# 0198. Declared Models on Preset Connections: Per-Instance User-Intent Model Lists

- **Status:** Accepted
- **Date:** 2026-09-08
- **Builds on:** ADR-0123 (provider instances are state; routes are derived, never persisted), ADR-0065 (runtime fitted-model capability overlay), ADR-0149 (capability resolution order: user > remote > baseline), ADR-0171 (three-layer model catalog and pluggable network sources)

## Context

Providers ship model ids that the catalog cannot legitimately serve: hidden
preview ids absent from the provider's own `GET /models` list, unstable
pre-release names that rotate faster than release cadence, and dated snapshot
ids that exist upstream before models.dev records them. A user with a working
API key and a known id has no path to use it on a preset connection today.

The gap has a precise shape in the code:

1. **Discovery is subtractive by design.** For a `ModelSource::Api`
   connection, the live fetch is intersected against the compiled baseline —
   `supported_model_intersection(&supported_models_for_preset(spec), &ids)`
   (`crates/muta-agent/src/catalog/discovery.rs:337`, `:530`). An id absent
   from the baseline is dropped even when the endpoint advertises it. This
   trust boundary is correct and stays: a relay or endpoint must never be able
   to fabricate channels (ADR-0065).

2. **`fitting` is the wrong lever.** `ProviderPresetSpec.fitting`
   (`crates/muta-providers/src/registry/mod.rs:175`) lets discovery materialize
   unknown ids — but only for first-party endpoints whose `/models` advertises
   real capability fields (Kimi Code). DeepSeek's endpoint returns bare ids
   with no metadata (`crates/muta-contracts/src/effort.rs:486`), so fitting can
   never apply, and a hidden id absent from the list is unreachable regardless.

3. **Custom connections are the only declaration surface, and it is
   connection-shape-wide.** `Connection.models`
   (`crates/muta-persistence/src/connections.rs:67`) is explicitly reserved:
   *"Preset connections never set this."* A user who wants one hidden model on
   their deepseek connection must instead create a separate pure-custom
   connection — duplicating the credential, losing the preset's derived
   routing (DeepSeek Responses dialect), prompt-cache spec, and discovery.

4. **The preset tables are the wrong scope.** Adding the id to
   `DEEPSEEK_BUILTIN_MODELS` / the baseline `MODELS` table works, but ships
   every user's binary with the id, touches the shared registry tests
   (`preset_models_are_covered_by_the_local_baseline_table`,
   `registry/mod.rs:579`), and cannot be scoped to one account or connection.

The capability half already exists. ADR-0149's resolution order —
`capability_overrides` (user) → `RemoteModelMetadata` (remote) → baseline —
resolves arbitrary ids gracefully
(`ModelCapabilities::for_channel`, `crates/muta-contracts/src/model.rs:295`),
and the per-model `e` editor already writes layer-1 overrides for any channel.
What is missing is only the *existence* half: nothing can put an unknown id on
a preset connection's derived route set.

### The ADR-0123 tension, named

ADR-0123's headline rule is "connections carry no model-list state: routes are
derived, never persisted." That rule exists so two connections of one preset
never duplicate or drift a *derived* channel set — the model list is a
function of (preset, discovery cache), and re-derivable state must not be
copied. A user-declared model is not re-derivable: no derivation step can
reconstruct the user's intent to pin `deepseek-v4-pro-preview-0912` to one
connection. It is intent, the same class of state as `FittedModelInfo`
(ADR-0065), `RouteSettings` (ADR-0149), and the session provider pin — all of
which live in state stores as documented exceptions. The rule is refined, not
violated: *derived* model-list state stays off the connection; *declared*
model-list state belongs on it.

## Decision

1. **`Connection.extra_models: Vec<DeclaredModel>`** — a new field on the
   connection record in `connections.toml`
   (`crates/muta-persistence/src/connections.rs`), holding
   user-declared models for preset connections:

   ```toml
   [[connections]]
   id = "deepseek-personal"
   preset_id = "deepseek"
   # ...
   [[connections.extra_models]]
   id = "deepseek-v4-pro-preview-0912"
   context_window = 1_000_000
   ```

   `DeclaredModel` carries the exact (case-sensitive) model id plus optional
   capability facts (`context_window`, `max_output_tokens`, `thinking`,
   `vision`, `tool_call`). `Connection.models` stays reserved for pure-custom
   declarations; the two fields never mix.

2. **Union at derive time, per connection.** `route_models`
   (`crates/muta-agent/src/catalog/derive.rs:68`) appends the connection's
   `extra_models` ids after the derived (discovered or snapshot) set — deduped
   against it, declaration order preserved. The union happens *after*
   discovery, so a discovery refresh that omits the hidden id can never evict
   it, and the subtractive trust boundary is untouched: discovery still cannot
   add channels, the user can. Scoping is per-instance by construction: the
   field lives on the connection record, not the preset spec, so a second
   deepseek connection with empty `extra_models` derives nothing extra.

3. **Declared capabilities materialize as `RemoteModelMetadata` on the
   channel.** `derive_channel` (`derive.rs:89`) builds a `RemoteModelMetadata`
   from the declared fields and stamps it as the channel's `remote`. This rides
   the existing ADR-0149 merge — declared fields win, undeclared fields fall
   through to the registry default — with zero new capability plumbing, and the
   user's later per-route fine-tuning still stacks on top as layer-1
   `capability_overrides` (`Channel::capabilities`,
   `crates/muta-contracts/src/catalog.rs:218`). Routing needs no new code:
   `preset_route(pid, model)` falls through to the preset's protocol/endpoint,
   and the DeepSeek Responses dialect special-case in `base_route` is keyed on
   `preset_id`, so an extra model rides the same wire as the preset's own ids.

4. **Runtime intents, one per mutation.** `AgentRequest::AddConnectionModel`
   and `AgentRequest::RemoveConnectionModel` (keyed by `connection_id`),
   handled in `crates/muta-runtime/src/handlers_provider.rs` mirroring the
   existing `remove_model` handler: validate (non-empty sanitized id, no
   exact duplicate of an existing declaration — case-variant ids are distinct
   on the wire, consistent with `custom_baselines` — connection exists and
   `is_preset()`), mutate the store, persist, prune stale picker
   state, and push a fresh `ProviderPicker` snapshot (re-activating the live
   provider when the edited connection is the active one and the added model
   becomes relevant). Both frontends go through the wire intents; nothing
   edits `connections.toml` by side door.

5. **Removal is symmetric and cheap.** `RemoveConnectionModel` drops one
   declared id. It does not gate on "last model" (unlike pure-custom
   `remove_model`): a preset connection always has its derived set to fall
   back on. Removal also clears the id from `config.default_model` /
   favorites scope via the existing `prune_stale_models` path.

## Alternatives considered

- **Add the id to the compiled preset tables** (`DEEPSEEK_BUILTIN_MODELS` +
  baseline `Model`). Rejected as the general mechanism: global to all
  installations and all instances of the preset, requires a release per hidden
  id, and drags the shared-registry fidelity tests along. It remains the right
  tool when a model *graduates* — stable, documented, capability-verified — at
  which point the declaration is deleted and the baseline takes over.

- **Enable `fitting` on the deepseek preset.** Rejected: fitting is a trust
  decision reserved for endpoints that advertise capability fields
  (`registry/mod.rs:181`); DeepSeek's `/models` returns bare ids, so fitting
  would materialize capability-less channels, and a hidden id absent from the
  list is still unreachable. It also confuses two sources: endpoint
  advertisement vs. user intent.

- **Reuse `Connection.models` for preset connections.** Rejected: the field's
  contract (pure-custom declarations; empty for preset connections,
  `declared_models()`) is load-bearing in `add`/`edit`/`remove_model` and the
  catalog's route derivation. Overloading it would make "which models are
  derived vs declared" unparsable from the record.

- **Presence-in-`RouteSettings` as existence.** Rejected: the route-settings
  store is keyed per (connection, model) *knobs*; conjuring a picker row from
  a settings entry's presence conflates capability preferences with model
  existence and breaks `is_empty()` pruning.

- **A separate user-models store keyed by (preset, model).** Rejected: it
  splits the connection's identity across two files and re-introduces exactly
  the drift ADR-0123 deleted (per-instance copies of model state). The
  connection is the security master; its model set belongs with it.

## Consequences

- **Positive.** One hidden model on one connection is a user operation, not a
  release; discovery stays purely subtractive; capabilities flow through the
  single ADR-0149 pipeline; routing, prompt-cache spec, and dialect come free
  from the preset; the feature composes with every preset, not only deepseek.
- **Neutral.** `connections.toml` grows one optional field; connections
  without it serialize byte-identically (`skip_serializing_if`). Model ids
  added this way are exact matches — case-variant ids resolve as distinct
  models, consistent with `custom_baselines` behavior.
- **Negative.** A declared id with no capability facts resolves to registry
  defaults (small context window) until the user declares them or the model
  graduates to the baseline — mitigated by the add-model form surfacing
  `context_window` first, and by the model `e` editor for later correction.
  A stale declaration whose id later vanishes upstream keeps a dead route —
  accepted: it is the user's pin, removable at will, and indistinguishable
  from the user knowing something the catalog does not.
- **Migration.** None: additive serde field with a default. Tests: roundtrip
  of `extra_models`, derive union order/dedupe, capability overlay through
  `RemoteModelMetadata`, discovery-refresh non-eviction, handler
  validate/persist/push paths.

## References

- ADR-0123 — instances-are-state/routes-are-derived; the rule this ADR refines
  with a declared-state exception.
- ADR-0065 — fitted-model overlay; the precedent for non-derivable model state
  in a persistence store, and the trust boundary this ADR preserves.
- ADR-0149 — capability resolution order; the pipeline declared capabilities
  ride.
- ADR-0171 — three-layer catalog and `LiveCatalog`; the discovery layer this
  ADR deliberately leaves subtractive.
- `crates/muta-agent/src/catalog/derive.rs` — `route_models` (union site),
  `derive_channel` (metadata stamp site).
- `crates/muta-persistence/src/connections.rs` — the connection record and the
  reserved `models` contract.
- `crates/muta-runtime/src/handlers_provider.rs` — `remove_model`; the handler
  shape mirrored.
- `crates/muta-providers/src/registry/mod.rs` — `ProviderPresetSpec.fitting`
  trust contract; `docs/dev/new-model-onboarding.md` — the baseline-graduation
  path this ADR complements.
