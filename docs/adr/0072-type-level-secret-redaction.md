# ADR-0072: Type-level secret redaction (SecretString)

- **Status:** Accepted
- **Date:** 2026-07-23

## Context

Provider API keys and OAuth tokens were held as plain `String` inside types
that `derive(Debug)`: `Config` / `UserProviderConfig` / `UserChannelConfig`
(`neenee-store/src/config.rs`), the `AgentRequest::{SwitchProvider,
AddProvider, EditProvider}` wire variants (`neenee-core/src/events.rs`), and
the OAuth token structs in `neenee-auth`. Any `{:?}` formatting of these —
a log line, an error chain, a panic message — would print credentials in
cleartext. A second concrete leak existed in the provider HTTP layer:
Gemini authenticates via a `?key=` URL query parameter, and
`transport_error()` formatted `reqwest::Error` with `{}`, whose `Display`
embeds the request URL — so a Gemini transport failure could print the key
into user-facing errors and logs.

On-disk files (`config.toml` / `credentials.toml` / `auth.toml`, mode 0600)
are intentionally plaintext; the threat model here is *transient* exposure
through logs and error text, not at-rest storage.

## Decision

Introduce `SecretString` in `neenee-core` (`secret.rs`) and use it for every
credential-holding field:

- `Debug` renders `SecretString(***)` and `Display` renders `***`;
  `expose_secret()` is the only way to read the plaintext.
- Serde is transparent (`#[serde(transparent)]`), so the serialized shape is
  byte-identical to a plain `String` — existing config and credential files
  load unchanged, zero migration.
- Applied to: `UserChannelConfig.api_key`, the built-in provider `*_api_key`
  fields and `Credentials` values (`neenee-store`), OAuth token responses /
  token sets / PKCE verifier / device codes (`neenee-auth`), web-search
  provider keys (`neenee-core/src/webconfig.rs`), and the provider-management
  `AgentRequest` variants.
- The ai-sdk HTTP crates keep their plain `String` signatures: their structs
  are verified to not `derive(Debug)`, and the plaintext crosses the boundary
  only at provider construction via short-lived `expose_secret()` borrows.
- `transport_error()` (`neenee-ai-sdk-core`) additionally masks the values of
  credential query parameters (`key` / `api_key` / `apikey` / `access_token`)
  in any URL embedded in a transport error message.

## Alternatives considered

- **Hand-written redacted `Debug` impls per struct.** Rejected: the
  discipline is per-struct and every new credential-holding struct must
  remember to opt in; a newtype makes the safe behaviour the default at the
  type level.
- **The `secrecy` crate.** Rejected: the same pattern fits in ~100 lines
  with no new dependency, matching the workspace's dependency budget.
- **Move Gemini auth to the `x-goog-api-key` header.** Rejected for now: it
  drops the key from URLs entirely, but compatibility with Gemini-compatible
  relays that only accept `?key=` is unverified. Query masking closes the
  error-path leak today; the header switch remains a future option.

## Consequences

- Config, auth, and wire types are now safe to `{:?}`; a workspace-wide grep
  of `{:?}` / tracing macros against config/channel/token/key finds no
  cleartext path.
- `expose_secret()` remains available by design; keeping it rare and
  short-lived is a code-review discipline, not a type-system guarantee.
- The Gemini key still travels in the URL query (upstream server logs see
  it) — inherent to query-based auth; only the error-formatting path is
  masked.
- `McpServerConfig.environment` is a free-form user env table, not a
  credential field; secrets a user places there stay their responsibility.

## References

- [ADR-0071](0071-defer-kernel-split-and-backport-strictness.md) — the
  back-port pass this hardening belongs to.
- praxion `shared/secret.rs` — the pattern's origin in the fork.
