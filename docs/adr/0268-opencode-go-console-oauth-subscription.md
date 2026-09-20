# 0268. OpenCode Go as an OpenCode Console OAuth Subscription: JSON Device-Authorization Grant, Declarative Preset, and Load-Time Credential Migration

- **Status:** Accepted
- **Date:** 2026-09-20
- **Implementation:** `muta-contracts`, `muta-providers`, `muta-persistence`, `mutx`
- **Builds on:** [ADR-0201](0201-model-provider-service-surface-and-connection-identity.md), [ADR-0258](0258-first-class-declarative-model-providers-and-connection-pipe-purity.md), [ADR-0267](0267-transport-middleware-pipeline-and-extensible-credential-architecture.md)
- **Supersedes:** the `opencode-go` API-key authentication template

---

## Context and Problem Statement

`opencode-go` is the OpenCode Go / Zen relay surface
(`https://opencode.ai/zen/go/v1`), whose catalogue is served from the private
`models.opencode.ai` endpoint. It was modelled as an **API-key** connection: the
user pasted a key and the runtime resolved it as a static bearer.

Upstream OpenCode has since made the **OpenCode Console account** the primary
credential for the same relay. `packages/core/src/plugin/provider/opencode.ts`
and `packages/opencode/src/account/account.ts` define an OAuth
device-authorization grant against the console origin (`https://opencode.ai/console`,
client id `opencode-cli`):

- `POST /auth/device/code` (JSON `{client_id}`) returns
  `device_code`, `user_code`, `verification_uri_complete`, `expires_in`, `interval`.
- `POST /auth/device/token` (JSON, grant
  `urn:ietf:params:oauth:grant-type:device_code`) returns the token set or an
  `authorization_pending` / `slow_down` / `access_denied` / `expired_token` error.
- The same `/auth/device/token` endpoint rotates the token on
  `grant_type=refresh_token`.
- `GET /api/user` and `GET /api/orgs` identify the account and its workspaces.

This grant is **not RFC 8628**: it speaks JSON, returns a fully-formed
`verification_uri_complete` (no separate `verification_uri`), and reuses the
device-token endpoint for refresh. The generic `oauth::device` path
(form-urlencoded, `verification_uri` required) cannot serve it.

Two problems follow. First, muta offered no way to sign into the Go plan with an
account. Second, keeping the API-key template alongside a new OAuth method would
reintroduce the dual-auth special-casing ADR-0267 is explicitly dismantling
(`ConnectionAuth` variants, per-provider branches, dual-write credential
resolution).

---

## Decision

1. **OpenCode Go is an OAuth-only subscription surface.** The `opencode-go`
   connection template authenticates with
   `ConnectionAuth::subscription("opencode")`; there is no API-key template for
   this provider. The model provider identity (`opencode-go`), relay routes, and
   `models.opencode.ai` catalogue are unchanged — only the credential mode moves.

2. **The grant is a first-class, declaratively-described OAuth preset**
   (`oauth::presets::opencode_preset`): JSON token format, device-only
   (`browser_login = false`), no PKCE, no scope. The non-standard device flow is
   encapsulated in `oauth::opencode_device`, dispatched by the existing
   `DeviceFlowMode::Custom("opencode")` seam — no provider-name branch leaks into
   the orchestrator.

3. **Account metadata is enriched polymorphically** via
   `OpencodeOAuthEnricher` (ADR-0267): it records `account_id`, the default
   `org_id`/`org_name`, and the full `opencode_orgs` membership on login and
   preserves them across refresh.

4. **`ChatGptAuthMetadata` is attached only for the ChatGPT subscription
   integration.** A generic `account_id` token attribute no longer projects a
   non-ChatGPT connection onto Codex's `chatgpt-account-id` surface.

5. **Persisted API-key `opencode-go` connections migrate on load** to
   `ConnectionAuth::subscription("opencode")` (`connections::migrate_connection_auth`).
   The migration is idempotent and scoped strictly to the `opencode-go` provider.
   The now-unreachable `credentials.toml` entry is left untouched; the connection
   prompts for sign-in on next activation.

---

## Alternatives considered

- **Keep API-key auth as the default and add OAuth beside it.** Rejected: it
  reintroduces the dual-auth special-casing ADR-0267 exists to remove, and no
  other subscription surface offers a key path for the same account identity.
- **Reuse the generic RFC 8628 device flow.** Rejected: the grant is JSON and has
  no `verification_uri`; forcing it through the generic path would either break
  or require a provider-name branch inside the generic flow.
- **Force a hard break with no migration (require users to re-add the
  connection).** Rejected: a one-line, idempotent load migration is strictly
  cheaper and leaves no stale `ApiKey` records.
- **Fetch the account's provider/model configuration from `GET /api/config`
  (full upstream parity).** Deferred: it is a provider-catalogue override
  mechanism, not an authentication concern. muta's compiled provider spec already
  supplies the relay endpoint, and the keyless `models.opencode.ai` catalogue is
  already authoritative for the served models. It can be layered on later without
  changing this decision.
- **An interactive org picker.** Deferred: upstream itself selects the first
  org deterministically and only needs the workspace for `/api/config`, which
  this ADR does not adopt. The full membership is persisted so a picker can be
  added without another login.

---

## Consequences

- **Positive.** The Go plan signs in with the same account as upstream; no
  client secrets or pasted keys are stored for this provider; the new surface is
  ~a preset + a device module + an enricher, with no core-engine edits
  (ADR-0267's Open-Closed property holds).
- **Positive.** Existing `opencode-go` connections are upgraded automatically and
  cannot silently keep using a retired credential mode.
- **Negative.** An upgraded connection requires a one-time interactive sign-in
  before it can serve inference. The old `credentials.toml` entry is orphaned
  (inert, but not garbage-collected).
- **Neutral.** Org selection and `/api/config` remain follow-ups; the persisted
  `opencode_orgs` membership and `org_id` attribute make them additive.

---

## References

- [ADR-0201](0201-model-provider-service-surface-and-connection-identity.md) — model provider identity is independent of authentication mode.
- [ADR-0258](0258-first-class-declarative-model-providers-and-connection-pipe-purity.md) — first-class declarative model providers.
- [ADR-0267](0267-transport-middleware-pipeline-and-extensible-credential-architecture.md) — extensible credential architecture, `OAuthTokenEnricher`.
- Upstream: `packages/core/src/plugin/provider/opencode.ts`, `packages/opencode/src/account/account.ts`, `packages/opencode/src/account/url.ts`.
