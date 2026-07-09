# 0052. xAI (Grok / SuperGrok) provider

- **Status:** Accepted
- **Date:** 2026-01-01

## Context

We wanted an xAI provider, mirroring opencode's SuperGrok integration
(`/home/ming/projects/opencode/packages/opencode/src/plugin/xai.ts`). xAI's Grok
4.x family is OpenAI-compatible (chat completions at `https://api.x.ai/v1/chat/
completions`), so it slots into the existing `OpenAiCompat` transport without a
new SDK crate. The wrinkle is authentication: neenee had **no OAuth
infrastructure at all** — every provider authenticated with a static API key
resolved `env > credentials.toml > inline config`. SuperGrok subscriptions,
however, authenticate via OAuth2 (PKCE + RFC 8628 device code + refresh-token
rotation), reusing xAI's public Grok-CLI OAuth client because xAI rejects
loopback OAuth from non-allowlisted clients.

## Decision

1. **Register the Grok model family** in `neenee-core/src/model.rs`
   (`grok-4.5`, `grok-4.20`, `grok-4.3`, `grok-build-0.1`), all
   `WireFormat::OpenAiCompat`, multimodal, with a new `EFFORT_XAI_GROK`
   effort set (`none`/`low`/`medium`/`high`). Add `XAI_BUILTIN_MODELS` and a
   `"xai"` entry to `builtin_provider_metadata`. Add an "xAI" template to the
   TUI provider chooser. This is the **API-key path**: a user with an
   `XAI_API_KEY` adds the provider and pastes the key, like any OpenAI relay.

2. **Add a new `neenee-auth` crate** (`crates/neenee-auth`) that ports
   opencode's xAI OAuth flow to Rust: PKCE S256 (`pkce.rs`), the
   authorize/exchange/refresh token-endpoint helpers + JWT-`exp` proactive
   refresh (`token.rs`), the RFC 8628 device-code grant with `authorization_
   pending`/`slow_down` handling (`device.rs`), the pinned `127.0.0.1:56121`
   loopback callback server with CSRF state validation (`browser.rs`), and an
   atomic `auth.toml` (0600) token store keyed by provider id (`store.rs`). A
   `XaiOAuth` facade ties them together with **single-flight refresh** so a
   rotating refresh token is never replayed across concurrent channel builds.

   The load-bearing xAI details are constants: the public Grok-CLI
   `CLIENT_ID`, the `auth.x.ai` endpoints, the `grok-cli:access api:access`
   scope, and the `plan=generic` query param (without which `accounts.x.ai`
   rejects loopback OAuth from the reused client).

3. **Wire OAuth into the config + catalog** via a new
   `neenee_core::ChannelAuth` discriminator (`ApiKey` | `XaiOAuth`) on
   `UserChannelConfig`. The catalog's `user_channel_to_channel` resolves a
   `XaiOAuth` channel's bearer from `auth.toml` at build time (ignoring the
   inline key). The xAI template seeds `XaiOAuth` channels.

4. **OAuth-first add flow + re-connect** — the template chooser lists
   **xAI OAuth** as its own auth-method entry (future: API-key / other schemes
   as separate entries). Selecting it sends `AgentRequest::AuthorizeOAuth`
   (`LoginMethod::Browser`): the harness binds `127.0.0.1:56121`, streams
   `ConnectStatus::Pending` (URL auto-opened + shown for manual click), waits
   for the loopback callback, and persists tokens under `auth.toml` key `xai`.
   On `Done`, the TUI opens a **name-only** editor so the user names the
   instance, then `AddProvider` creates the channels. Re-selecting an existing
   OAuth instance with no/expired token uses `ConnectProvider` (browser) then
   activates. No standalone `login` command.

## Alternatives considered

- **xAI as a new `Transport` variant.** Rejected: Grok speaks vanilla OpenAI
  chat completions, so the existing `OpenAiCompat` transport reaches it
  unchanged. A new transport would duplicate the request/response logic.
- **Reuse an OAuth crate (e.g. `oauth2`).** Rejected: the xAI flow has
  specifics (the reused client id, `plan=generic`, RFC 8628 device grant) that
  a generic crate obscures, and opencode's hand-rolled flow is small and
  battle-tested. A ~600-line dedicated crate is clearer and dependency-light.
- **Resolve the OAuth token lazily inside `OpenAiCompatProvider` (refresh on
  each request).** Deferred: the catalog-resolves-stored-token approach is the
  minimal MVP that "just works" after a connect. A request-time refresh
  (re-authenticating transparently when the stored token expires) is a future
  enhancement; today, re-selecting the provider in the picker re-runs connect.

## Consequences

- **Positive.** xAI works both with an API key (API-key template) and a
  SuperGrok subscription (OAuth template), using the **same** picker operation
  as every other provider — select → activate (with a one-time connect for
  OAuth). The OAuth crate is reusable for any future OAuth-only provider.
  `ChannelAuth` is a clean discriminator that doesn't disturb the existing
  API-key path (default).
- **Negative.** Token refresh runs at activate/switch (not on every request).
  A long-running session whose access token expires mid-flight recovers on the
  next provider switch (or re-select). A failed refresh clears `auth.toml` so
  the picker re-routes to ConnectProvider. The device-code poll runs inline on
  the agent loop (a connect blocks request processing until the user authorizes),
  which is acceptable because connect is an explicit user action, not part of a
  chat turn.
- **Migration.** None — additive. `paths::set_test_default` /
  `TEST_OVERRIDE_GUARD` were un-`#[cfg(test)]`'d in `neenee-store` so
  downstream integration tests (the OAuth catalog test) can sandbox the paths.

## References

- opencode reference: `packages/opencode/src/plugin/xai.ts`,
  `packages/llm/src/providers/xai.ts`.
- Related: ADR-0002 (channel abstraction), ADR-0046 (per-model reasoning).
- xAI docs: `https://docs.x.ai/developers/models`.
