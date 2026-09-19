# 0260. Provider dialect inheritance

- **Status:** Accepted
- **Date:** 2026-09-19
- **Implementation:** `muta-contracts`, `muta-providers`, `muta-persistence`, `muta-agent`, `muta-runtime`
- **Builds on:** [ADR-0258](0258-first-class-declarative-model-providers-and-connection-pipe-purity.md)
- **Amends:** [ADR-0259](0259-deterministic-root-url-algebra-and-model-level-wire-protocol-inheritance.md), inference endpoint composition

## Context

The provider refactor removed the Antigravity OAuth routing branch. Model
protocol selection still resolved Google Gemini correctly, but the common route
assigned the Generative Language dialect. An internal Google service consequently
received a public API path and returned HTTP 404. Declarative providers already
accepted a dialect string, but runtime registration discarded it.

## Decision

Model protocol and provider dialect are independent inputs. A model selects its
wire protocol within its provider; absent remote protocol metadata inherits the
provider-scoped model baseline, then the provider default. A global model ID is
not authority for another provider's route. Existing explicit provider model wire
entries remain part of the provider-scoped baseline.

Both built-in and declarative providers carry the same typed service dialect.
Channel derivation selects the protocol-specific dialect from that declaration.
Authentication selects credential acquisition and refresh only. It cannot select
a wire protocol, dialect, or endpoint.

Endpoint selection follows the effective model protocol. Services with differing
protocol endpoints declare those endpoints as provider data. Adapter path
composition depends on both protocol and dialect, amending ADR-0259's
protocol-only formula: `root + inference_path(protocol, dialect, model)`.
Antigravity composes `v1internal:generateContent` or its streaming equivalent;
Generative Language composes `models/{model}:generateContent` or its streaming
equivalent. Existing adapter endpoint representations remain unchanged.

A service dialect requiring a specific envelope rejects incompatible model
protocols with a route error before credential resolution or HTTP dispatch.
Catalog construction logs and excludes invalid channels while retaining valid
channels. Direct route consumers receive the error. No protocol guessing, model
substitution, or panic is permitted.

## Invariants

- A remote protocol declaration never erases the provider dialect.
- An omitted remote protocol uses provider-scoped metadata and defaults.
- Channel derivation contains no provider-name or authentication-based dialect
  and endpoint branches.
- Declarative and built-in providers use the same dialect representation.
- An HTTP 404 alone does not establish model deprecation or unavailability.

## Rejected alternatives and negative knowledge

- Restoring an Antigravity OAuth branch would tie service semantics to credential
  acquisition and leave declarative providers broken.
- Writing Google protocol entries for every remote model would duplicate the
  provider default and require maintenance for new model generations.
- Replacing model protocol autonomy with one fixed provider protocol would break
  services such as Copilot that advertise multiple protocol families.
- Retrying different protocols or substituting an older model after HTTP 404
  would hide configuration defects and change the requested service behavior.

## Consequences and verification

Dialect values are validated as typed configuration data. Unknown dialect names
fail deserialization or registration instead of being silently ignored. The
channel derivation API returns a result so malformed routes cannot panic.

Tests cover provider defaults, explicit remote protocol selection, service-local
baselines, incompatible protocols, and persisted declarative Antigravity
configuration through OAuth resolution and actual mock HTTP dispatch for both
streaming and non-streaming requests.
