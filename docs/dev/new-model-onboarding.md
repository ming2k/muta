# New-Model Onboarding Checklist

Use this checklist when a provider platform launches a new model (or a new
model tier) that this client should offer. It exists because `glm-5.3-flash`
shipped on Zhipu's coding plan and the client's model list lagged behind —
each gap below is a place that lag was visible to a user.

## Why this is easy to get wrong

The model list a user sees is assembled from several layers, each with its
own home. No single edit makes a model appear everywhere:

1. **Provider baseline table** — `crates/muta-providers/src/registry/<provider>.rs`,
   the `MODELS: &[Model]` const. This is the single source of *capability*
   truth (context window, vision, thinking style, effort ladder, wire format).
2. **Offering list** — the same file's `<PROVIDER>_MODELS: &[&str]` (e.g.
   `ZAI_CODE_MODELS`): the curated, ordered ids the preset seeds as channels
   and shows in the model picker.
3. **Fidelity snapshot** — `registry/baseline_fidelity_tests.rs::PRE_MIGRATION`,
   the frozen pre-migration registry. New models are *not* added here (it is a
   one-way snapshot); skip it unless you are intentionally re-baselining.
4. **Reference docs** — `docs/reference/providers.md` (preset table row +
   bullet), and any family listing in `docs/reference/`.
5. **`CHANGELOG.md`** — an `Unreleased` entry under the right section.

## Checklist

- [ ] Confirm the platform actually serves the model: hit the provider's
      `GET /models` (with a real key) and/or the platform's model-card page.
      Record *where* the capability numbers came from (URL) in the baseline
      entry's comment block.
- [ ] Add the `Model { .. }` entry to the provider's `MODELS` table with a
      comment stating the capability source and any quirks (e.g. "always-on
      thinking", "ids only, no metadata from /models").
- [ ] Add the id to the provider's offering list, positioned deliberately:
      flagship first, then new tier models, then older flagships. The first
      entry is the initial active channel.
- [ ] If the id is **already declared by another provider file** (shared ids
      like `glm-5.2`, `kimi-k2.7-code`): the copies must be field-identical —
      `shared_baseline_ids_are_identical_across_provider_tables` enforces it.
      Copy the existing entry rather than re-deriving it.
- [ ] Run `cargo nextest run -p muta-providers` — the coverage test
      (`template_models_are_covered_by_the_local_baseline_table` — historical test name) fails loudly
      if the offering list and the baseline table drift apart.
- [ ] Update `docs/reference/providers.md`: the preset table row (model list
      column) and the provider bullet (behavior, discovery, defaults).
- [ ] Add a `CHANGELOG.md` `Unreleased` entry describing what a user gains.
- [ ] If the platform's `/models` endpoint now returns capability metadata it
      did not before, revisit `fitting:` on the preset spec (see
      [ADR-0070](../adr/0070-provider-scoped-remote-model-metadata.md)):
      ids-only endpoints stay `fitting: false` (baselines own capabilities);
      trusted rich-metadata endpoints may flip to `fitting: true`.

## Layer weights (why the checklist looks like this)

Effective capability resolution for a model id is:

```
user config (RouteSettings, per provider-instance + model id)
  > remote advertised metadata (fitting: true presets only)
  > provider baseline table (this checklist's layer 1)
  > (visibility only) live /models discovery intersection
```

A new model therefore needs a baseline entry even when discovery is on: the
Zhipu coding `/models` endpoint returns ids only (`{id, object, created,
owned_by}` — verified live 2026-08), so without a baseline the model would be
visible but capability-less, or filtered out entirely by the intersection.

## Anti-patterns

- Adding an id to the offering list without a baseline entry (the picker
  would offer a channel that resolves no capabilities).
- Editing `PRE_MIGRATION` to "add" a model — it is a frozen snapshot of the
  pre-migration registry; adding entries weakens its fidelity guarantee.
- Duplicating a shared id's baseline with re-derived (slightly different)
  fields instead of copying the existing entry byte-for-byte.
