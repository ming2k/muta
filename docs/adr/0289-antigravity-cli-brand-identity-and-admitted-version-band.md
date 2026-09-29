---
id: ADR-0289
title: "Antigravity CLI Brand Identity Is Load-Bearing: The `antigravity/cli/<version>` User-Agent Admission Band"
status: accepted
date: 2026-09-28
scope: contracts/client-identity, providers/oauth, providers/catalog, llm-client/google
superseded_by: null
negative_knowledge: true
---

# 0289. Antigravity CLI Brand Identity Is Load-Bearing: The `antigravity/cli/<version>` User-Agent Admission Band

- **Status:** Accepted
- **Date:** 2026-09-28
- **Scope:** `muta-contracts` (`client_identity`), `muta-providers` (`oauth`, `list_models`), `muta-llm-client` (`protocol::google`)
- **Deciders:** Muta Architecture Team
- **Builds on:** [ADR-0164](0164-first-class-client-profiles-and-connection-emulation.md) (First-class client profiles and connection emulation contracts)

---

## Context and Problem Statement

The `google-antigravity` channel (Google One / personal consumer subscription
over the Cloud Code `v1internal` surface) stopped completing inference. Catalog
discovery, quota, eligibility, and onboarding all kept working — `loadCodeAssist`,
`fetchAvailableModels`, `retrieveUserQuotaSummary`, and `listExperiments` all
returned `200` — but every `v1internal:generateContent` call failed with
`404 NOT_FOUND`.

muta advertised the identity

```
User-Agent: antigravity/1.23.2 linux/amd64
x-goog-api-client: gl-go/1.23.2 gdcl/0.1
```

which had been transcribed from an earlier Antigravity build. The upstream
Antigravity CLI (`agy`) shipped an update that changed its self-identification:
the inference backend now discriminates on a **product brand segment** in the
User-Agent and admits only the CLI surface.

Because the failing endpoints are exactly the inference endpoints while every
metadata endpoint still succeeds, the defect presents as a partial outage that
looks like a quota or entitlement problem. It is not: it is a client-identity
admission band.

## Evidence

### Captured wire identity

Intercepting the updated `agy` (reported version `1.2.12`) with a local
TLS-terminating capture (`CLOUD_CODE_URL` + `AGY_CA_CERT`), every Cloud Code
request carried:

```
POST /v1internal:loadCodeAssist HTTP/1.1
Host: daily-cloudcode-pa.googleapis.com
User-Agent: antigravity/cli/1.2.12 (aidev_client; os_type=linux; arch=amd64; cl=989022288; auth_method=consumer)
Content-Type: application/json

{"metadata":{"ideType":"ANTIGRAVITY"}}
```

Three deltas from the identity muta carried:

1. **A brand segment `cli` appears** — `antigravity/cli/<version>` instead of
   `antigravity/<version>`.
2. **A structured comment** replaces the bare `os/arch` suffix, reporting
   `aidev_client`, `os_type`, `arch`, a build counter `cl=`, and `auth_method`.
3. **`loadCodeAssist` sends only `{"ideType":"ANTIGRAVITY"}`** — the extra
   `ideVersion` / `ideName` / `platform` / `pluginType` fields muta sent are
   optional and no longer emitted by the CLI.

### Measured admission band

Probing `v1internal:generateContent` with a live subscription bearer token and a
controlled `User-Agent` isolates the discriminator (all other fields held constant):

| `User-Agent` | Result |
|---|---|
| `antigravity/1.23.2 linux/amd64` (muta before) | `404 NOT_FOUND` |
| `antigravity/1.2.12 linux/amd64` (no brand) | `404 NOT_FOUND` |
| `antigravity/cliX/1.2.12` | `404 NOT_FOUND` |
| `antigravity/cli/1.0.0` | `404 NOT_FOUND` |
| `antigravity/cli/1.1.0` | `404 NOT_FOUND` |
| `antigravity/cli/0.99.0` | `404 NOT_FOUND` |
| `antigravity/cli/1.2.0` | admitted (`429`) |
| `antigravity/cli/1.2.11` | admitted (`429`) |
| `antigravity/cli/1.2.12` | admitted (`200`) |
| `antigravity/cli/1.19.0` | admitted (`429`) |
| `antigravity/cli/2.0.0` | admitted (`429`) |
| `antigravity/cli/9.9.9` | admitted (`429`) |
| `antigravity/cli/1.2.12 (aidev_client; …; auth_method=consumer)` | admitted (`200`) |

`429 RESOURCE_EXHAUSTED` denotes *admitted to the inference cluster, throttled*;
`404 NOT_FOUND` denotes *not routed at all*. The band is therefore
`antigravity/cli/<semver>` with numeric version `>= 1.2`, and:
- the HMAC-verified `gk` token exchange completed the same matrix, confirming the
  admission decision is a property of the routing tier, not of metadata payloads.
- The body's `userAgent` field, the `x-goog-api-client` header, the `cl=` build
  counter, `os_type`, and `arch` are **not** discriminators — every combination
  of those that kept the `antigravity/cli/<v>=1.2` header shell was admitted.

### Credential cross-check (why the OAuth client was not the cause)

The updated binary logs in with a new public OAuth client
(`884354919052-…`, paired secret `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`), but:

- `884354919052-…` + its own secret → `invalid_grant` (client valid);
- `884354919052-…` + the other secret → `invalid_client` (*"client secret is
  invalid"*);
- a **stored** `google-antigravity` refresh token refreshed successfully against
  the legacy `1071006060591-…` client but returned `unauthorized_client` against
  `884354919052-…`.

The refresh token is bound to the client that minted it. Switching the bundled
client would therefore invalidate every existing connection and force a
re-login, without affecting the `404`. The OAuth client is orthogonal to this
defect.

## Decision

### 1. Advertise the CLI brand and a pinned version inside the admitted band

`muta-contracts::client_identity` now emits the brand-bearing form:

```rust
pub const ANTIGRAVITY_VERSION: &str = "1.2.12";
pub const ANTIGRAVITY_CLI_MIN_PRODUCT_VERSION: &str = "1.2";
pub const ANTIGRAVITY_USER_AGENT: &str =
    "antigravity/cli/1.2.12 (aidev_client; os_type=linux; arch=amd64; cl=0; auth_method=consumer)";
```

`ANTIGRAVITY_CLI_MIN_PRODUCT_VERSION` records the measured admissions floor as a
first-class constant so the invariant is testable rather than folklore.
The pinned `ANTIGRAVITY_VERSION` sits inside the admitted band, and
`antigravity_cli_user_agent(..)` composes the full string for callers that need
a different platform or `auth_method`.

### 2. Keep the existing OAuth client

The bundled `google-antigravity` preset retains `1071006060591-…`. Switching to
the new binary's client is not required to fix the defect and would break stored
tokens. The new `antigravity-cli` preset now carries the **corrected** pairing
`884354919052-…` + `GOCSPX-9YQW…` (extracted from the updated binary and verified
live), replacing a client id that the endpoint rejects as
*"The OAuth client was not found."*

### 3. Route every Antigravity surface through the shared constant

`list_models.rs` and `protocol::google` previously duplicated the
`x-goog-api-client` literal; both now read
`ANTIGRAVITY_API_CLIENT_HEADER`. `oauth::token`'s duplicated constants delegate
to `muta-contracts`. One definition, one wire identity.

### 4. Additive `aicode` scope

The consent scope gains `https://www.googleapis.com/auth/aicode` — the scope
every stored Antigravity token set already carries, and the one that authorises
the Cloud Code inference surface.

## Alternatives considered

### Re-verify identity by parsing the installed `agy` binary at runtime

Rejected. It couples request construction to an optional external binary, needs a
`--version` subprocess on the request path, and fails closed to a stale default
exactly when the binary is absent. The admission band is wide (any `>= 1.2`
numeric version is admitted, `9.9.9` included), so a pinned constant inside the
band is faithful without any runtime probing.

### Adopt the new OAuth client (`884354919052-…`)

Rejected for the default channel. Proven unnecessary: the `404`/`200` decision is
made entirely on the User-Agent, and the same bearer token that `404`d under the
old User-Agent returned `200` under the new one. Adopting the new client would
additionally invalidate every stored refresh token (`unauthorized_client`) and
force a re-login, turning a one-line identity fix into account-wide disruption.
The new pairing is retained where it belongs — on the explicit `antigravity-cli`
preset.

### Carry Google's rotating build counter in `cl=`

Rejected. `cl=0` and `cl=989022288` are both admitted; the field is a
build-internal counter the routing tier ignores. Pinning a real counter would
only manufacture a stale value that must be tracked forever.

### Emit only `{"ideType":"ANTIGRAVITY"}` in `loadCodeAssist` to match the CLI byte-for-byte

Rejected. The extra metadata fields are optional and the backend honours them
when present; keeping them preserves onboarding against stricter deployments.
The version field, however, is now sourced from `ANTIGRAVITY_VERSION` instead of
a hardcoded stale literal, so it cannot drift from the advertised identity again.

## Negative knowledge

- **A `404 NOT_FOUND` from `v1internal:generateContent` is not a model-not-found
  or quota condition.** It is the admission gate rejecting the client identity.
  Do not debug it as a catalog, entitlement, or project problem; check the
  User-Agent brand and version band first.
- **`429` is admission, not rejection.** Treating `429 RESOURCE_EXHAUSTED` as
  proof the identity is wrong inverts the diagnosis.
- **Do not infer the gating signal from an error payload.** The body and headers
  carry no admission hint; only the status-code transition across a controlled
  User-Agent matrix reveals it.
- **Do not switch the OAuth client to chase an inference failure.** Refresh
  tokens are client-bound; a client swap is an account-wide breaking change and
  was proven orthogonal here.

## Consequences

- **Inference restored** on the `google-antigravity` channel for Google One
  subscription accounts, with no re-login and no token migration.
- **The admission band is executable.** `antigravity_user_agent_matches_cli_admitted_form`
  pins the brand, the comment components, and asserts the pinned version is at or
  above `ANTIGRAVITY_CLI_MIN_PRODUCT_VERSION`, so a regression to the brand-less
  form fails the suite rather than silently `404`ing in production.
- **One wire identity.** The `x-goog-api-client` literal and the User-Agent
  constant each have exactly one definition.
- **Accepted residual risk.** If Google raises the floor above the pinned
  version, `ANTIGRAVITY_VERSION` must be bumped; the pin deliberately trades a
  small maintenance obligation for determinism and offline reproducibility.
