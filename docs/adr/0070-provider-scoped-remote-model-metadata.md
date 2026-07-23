# 0070. Provider-scoped remote model metadata

- **Status:** Accepted
- **Date:** 2026-07-17

## Context

The static model registry was originally treated as the definitive source for a
model id's capability and wire-format information. That works when a provider
does not offer a machine-readable model list, but it is wrong for a trusted
provider that does. A provider can expose the same id through different APIs,
with different context limits, tool support, or account-plan availability.

GitHub Copilot makes the mismatch concrete. Its `/models` response declares
picker eligibility, endpoint support, limits, tools, vision, and reasoning. A
single account can receive Chat Completions, Responses, and Messages models.
Treating the static registry as globally authoritative loses those distinctions
and can route a valid model to the wrong API.

ADR-0065 introduced fitted metadata for unknown ids. It intentionally did not
let an endpoint override a known id, which leaves provider-specific facts
unrepresented.

## Decision

Persist a trusted provider's advertised model description on each channel that
represents that provider-model route.

1. A remote description is scoped to one provider instance and one model. It
   contains only the fields the provider explicitly publishes.
2. Effective capabilities merge remote fields over the static baseline. Missing
   remote fields retain the static value; explicit remote `false` and empty
   effort lists override it.
3. Remote endpoint selection is channel-scoped. It selects Chat Completions,
   Responses, or Messages without changing the model id's global fallback
   format.
4. Only templates that explicitly trust fitting materialize remote capability
   metadata. Relays continue to use their live list as an availability signal
   and the static registry as their metadata source.
5. Copilot accepts only `model_picker_enabled` models and uses its advertised
   endpoint per model. Its snapshot survives restart and remains usable when a
   later refresh fails.
6. The static registry remains the fallback for offline startup, providers
   without a model list, untrusted relays, and remote fields that were omitted.

## Alternatives considered

- Keep the static registry authoritative for known ids. Rejected because it
  cannot represent account-specific endpoint routing or explicit remote
  capability changes.
- Replace the static registry entirely with remote discovery. Rejected because
  many providers do not publish a complete list and discovery can fail.
- Store remote metadata globally by model id. Rejected because one provider or
  account must not change another provider's behavior.
- Trust every relay's capabilities. Rejected for the same safety reason as
  ADR-0065: a relay can incorrectly inflate a limit or advertise unsupported
  input types.

## Consequences

- A provider that publishes a complete trusted model description can add or
  change models without requiring a client registry release.
- Copilot routes each selectable model to the endpoint it advertises and uses
  its own plan-specific capability values.
- The persisted provider configuration gains an optional channel-level remote
  metadata record. Existing configurations deserialize without migration.
- Existing callers that resolve a bare model id still receive static or fitted
  fallback metadata. Provider construction and live-session context limits use
  the channel-specific effective view.

## References

- [ADR-0065](0065-runtime-fitted-model-capability-overlay.md)
- [Model Metadata](../reference/model-metadata.md)
- [Providers](../reference/providers.md)
