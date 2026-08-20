# 0123. Provider instances are state; routes are derived, never persisted

- **Status:** Accepted
- **Date:** 2026-08-20

## Context

Provider connectivity used to live in `config.toml` as `[[providers]]` tables
with embedded `[[providers.channels]]` rows. Each channel duplicated the
provider's whole model/endpoint surface, so several structural problems
accumulated:

- **Config mixed behavior with production data.** `config.toml` is the
  user-edited, shareable, versionable behavior file, yet it also carried
  machine-derived state: model lists mirrored from templates, per-route
  capability metadata, and fitted fields. Every startup had to *reconcile*
  that state (`reseed_channels_from_models`) because the persisted copy was
  already stale — a maintenance loop over a lie.
- **Channels never related to their provider.** Credentials were scattered
  (per-channel `api_key`, a per-channel `api_key_env`, `credentials.toml`
  `[builtins.<id>]` / `[user.<id>]`, and legacy top-level `*_api_key` fields).
  The runtime `Channel` is flat and does not reference its instance. There was
  no single "this instance's credential" fact.
- **Multiple instances of one provider duplicated the route set.** Two
  `deepseek` instances each carried a full copy of the model list, and the
  copies drifted independently; there was no shared definition.

## Decision

Split provider connectivity across the same three surfaces the rest of neenee
already uses, and make routes a *derived* value.

1. **`config.toml` = behavior only.** `default_provider` / `default_model`
   *reference* instance ids; permissions, bash policy, TUI prefs, hooks,
   skills stay. No provider definitions, no credentials, no model lists. The
   legacy `[[providers]]`, `*_api_key`, and `[model_reasoning]` tables are
   removed from the `Config` type; a leftover legacy `config.toml` still
   parses (unknown keys are ignored) and is cleaned on the next save.

2. **Instances live in a state store** (`$XDG_STATE_HOME/neenee/providers.toml`).
   A `ProviderInstance` declares: `id`, `name`, `template_id`, `auth`, an
   optional `api_key_env` (a variable *name*), and — for pure-custom
   instances with no template — `transport` / `base_url` / `user_agent` and
   the declared `models`. The instance is the **security principal**: it owns
   exactly one credential.

3. **Credentials are keyed by instance** (`credentials.toml
   `[providers.<id>]`, one flat `[providers]` table). Resolution precedence is
   `api_key_env` env var > `credentials.toml` > empty. The legacy
   `[builtins.<id>]` / `[user.<id>]` split is gone.

4. **Routes are derived at runtime, never persisted.** The catalog derives one
   `Channel` per model from `instance → template (+ discovery cache)`: the
   template owns the transport/endpoint/user-agent (and per-model routing for
   `opencode-go`), the discovery cache owns the *facts* a model advertised
   (list, fitted capability, remote endpoint) plus the user's per-
   `(instance, model)` reasoning overrides (`route_settings`). Multiple
   instances of one template share the definition and differ only in
   identity, credential, and overrides — no duplication, no drift.

5. **One-shot migration.** A dedicated converter (in the catalog layer, which
   can see the template registry) reads the legacy `config.toml`
   `[[providers]]` + `[model_reasoning]` and legacy `credentials.toml` and
   writes the new stores. It runs idempotently on the first launch with a
   current build (and from the CLI's config/auth commands) and is then dead
   weight.

## Consequences

- `config.toml` can be shared, screenshotted, or diffed without leaking
  credentials or stale model lists.
- Adding a second instance of an existing provider is one `[[providers]]` row
  plus a credential — no channel replication.
- Discovery no longer mutates configuration; it writes the cache. A failed
  fetch keeps the last valid subset exactly as before.
- Pure-custom relays still declare their models explicitly; template
  instances never do.
- The `neenee auth` / `neenee config` CLI surfaces now read the state store,
  so they work before the daemon has ever run (they trigger the migration).

## Alternatives considered

- **Keep instances in a slimmed `config.toml`.** Rejected: the user explicitly
  wants `config.toml` to be behavior, not production data, and a dedicated
  state file matches the existing pattern (`sessions`, `provider_usage.json`,
  `trusted_projects.json` all live in state, not config).
- **Keep per-model reasoning in `config.toml` `[model_reasoning]`.** Rejected:
  it is per-route data (`(instance, model)`), so it belongs with the other
  per-route facts in the discovery cache; config stays behavior.
