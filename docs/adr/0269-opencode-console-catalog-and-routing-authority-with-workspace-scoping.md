# 0269. OpenCode Console Catalog and Routing Authority with Workspace Scoping on Every Wire

- **Status:** Accepted
- **Date:** 2026-09-20
- **Implementation:** `muta-contracts`, `muta-providers`, `muta-agent`, `muta-llm-client`, `mutx`
- **Builds on:** [ADR-0259](0259-deterministic-root-url-algebra-and-model-level-wire-protocol-inheritance.md), [ADR-0265](0265-declarative-wire-surfaces.md), [ADR-0266](0266-declarative-remote-catalog-descriptors.md), [ADR-0267](0267-transport-middleware-pipeline-and-extensible-credential-architecture.md), [ADR-0268](0268-opencode-go-console-oauth-subscription.md)
- **Supersedes:** [ADR-0268](0268-opencode-go-console-oauth-subscription.md) §1 (relay routes and `models.opencode.ai` catalogue retained) and its two deferred items (`/api/config` adoption, org selection)

---

## Context and Problem Statement

[ADR-0268](0268-opencode-go-console-oauth-subscription.md) moved `opencode-go` to
OpenCode Console OAuth but deliberately kept the compiled zen/go relay
(`https://opencode.ai/zen/go/v1`) and the keyless `models.opencode.ai`
catalogue, recording `/api/config` adoption as a deferred follow-up. That
split left the credential and the transport on different accounts: a Console
`st_…` bearer token sent to the zen/go route.

Every model on the connection failed with `OpenAI HTTP 401 Unauthorized
{"error":{"message":"Invalid API key.","type":"AuthError"}}`. An ablation
matrix over the two surfaces isolates the cause:

| Surface | bearer `st_…` | workspace header | Upstream result |
| :--- | :--- | :--- | :--- |
| `/zen/go/v1/chat/completions` | sent | – | **401 Invalid API key** |
| `/inference/openai/v1/chat/completions` | sent | – | 403 Workspace selection is required |
| `/inference/openai/v1/chat/completions` | sent | sent | **402 Insufficient funds** (authenticated) |

The zen/go relay keys authenticate as API keys and never accept a Console
credential; the Console inference surface accepts the Console credential but
rejects it until the request names a workspace. 402 is a billing state, not a
code state.

The same experiment falsified the catalogue assumption. The keyless
`models.opencode.ai` feed serves 36 zen-era models; the authenticated
`GET https://opencode.ai/console/api/config` serves the account's 74 models and
declares, per model, a `provider: {npm, api}` override that selects the
inference root and wire protocol. The two disagree on routing: `minimax-m2.5`,
`minimax-m2.7`, and `minimax-m3` carry **no** override on the Console surface
(openai-compatible chat), while muta's compiled baseline — copied from
models.dev, which mirrors the zen feed — pinned them to Anthropic `/messages`.
A public, unauthenticated, previous-generation catalogue cannot authorize
requests against an account-scoped surface.

---

## Decision Drivers

- The credential that authenticates a request must be the credential that
  selected the route; a surface and a token from different accounts guarantees
  a 401.
- Routing truth must come from the account, not from a public mirror of a
  retired surface.
- Workspace scoping is not an optional hint on this provider: without it every
  inference call returns 403.
- No provider name may appear in a generic transport or routing path
  ([ADR-0267](0267-transport-middleware-pipeline-and-extensible-credential-architecture.md));
  per-model roots must flow through the existing
  [ADR-0259](0259-deterministic-root-url-algebra-and-model-level-wire-protocol-inheritance.md)
  algebra rather than a new side channel.

---

## Decision

1. **The authenticated Console configuration is the single catalog and routing
   authority.** `opencode-go` discovers models from
   `GET {catalog_root_url}/api/config` with its bearer token, reading
   `config.provider.opencode`. The zen/go relay and `models.opencode.ai`
   coordinates are deleted from this surface: no compatibility path, no
   fallback fetch.

2. **The catalog shape becomes `CatalogShape::OpencodeConsole`** (serialized
   `opencode-console`), replacing `OpencodeGo` outright
   ([ADR-0266](0266-declarative-remote-catalog-descriptors.md)). The shape
   identifier feeds the catalog identity hash, so the compiled change
   invalidates every cached entry and forces a refetch without a migration.

3. **Inference routes are the Console surfaces.** Provider root
   `https://opencode.ai/inference/openai/v1`, with protocol-scoped roots
   `https://opencode.ai/inference/anthropic/v1` (Anthropic Messages) and
   `https://opencode.ai/inference/google/v1beta` (Google `generateContent`),
   composed by the [ADR-0259](0259-deterministic-root-url-algebra-and-model-level-wire-protocol-inheritance.md)
   algebra. Catalogue roots sit outside the inference origin, so
   `catalog_root_url` (`https://opencode.ai/console`) is declared separately
   from `root_url`.

4. **Per-model `provider.api` is a root override, not a URL.**
   `RemoteModelMetadata` gains `endpoint: Option<String>` carrying the
   advertised **API root**; `base_route` then applies
   `endpoint_for(dialect, root, protocol)` exactly as it does for the compiled
   spec. The advertised root never becomes a full request URL, so protocol
   composition stays in one place.

5. **`provider.npm` selects the wire protocol:** `@ai-sdk/anthropic` →
   Anthropic Messages, `@ai-sdk/google` → Google `generateContent`,
   `@ai-sdk/openai-compatible` → Chat Completions, `@ai-sdk/openai` →
   Responses. An unrecognised `npm` yields no protocol, and the model falls
   back to the provider default rather than to a guess. Effort ladders stay
   undeclared when the account config publishes no `reasoning_options`, so the
   curated baseline is not negated by an absent field.

6. **Workspace identity is typed credential metadata.**
   `OpencodeAuthMetadata { org_id }` is attached by the OAuth credential source
   from the stored `org_id` attribute
   ([ADR-0267](0267-transport-middleware-pipeline-and-extensible-credential-architecture.md)
   extension map), and the refresh enricher refetches the org list for
   credentials stored before this change, so a pre-existing login self-heals on
   its next refresh.

7. **The workspace is projected onto every outbound call.** The catalog request
   sends `x-org-id`; every inference request sends `x-opencode-org-id`, both
   presence-gated on the metadata. The two surfaces spell the header
   differently; that asymmetry is upstream contract, not a muta inconsistency.

8. **Credential carriers follow the probe, per wire.** Anthropic keeps
   `x-api-key` (the relay accepts it). Google's standard path stops putting the
   secret in the query string when the credential is org-scoped: the token
   travels in `x-goog-api-key` and the URL stays keyless. Chat Completions and
   Responses keep their bearer header. Session affinity
   (`x-opencode-session`, `x-opencode-request`) is retained, keyed on the
   `opencode.ai` base URL.

---

## Invariants & Behavioral Boundaries

- An account-scoped surface is discovered from an endpoint that presents that
  account's credential. A public mirror of a provider's model list must never
  become the routing authority for a subscription surface.
- `RemoteModelMetadata.endpoint` holds an **API root**. Nothing writes a
  complete request path into it, and nothing reads a path out of it; the
  [ADR-0259](0259-deterministic-root-url-algebra-and-model-level-wire-protocol-inheritance.md)
  algebra alone composes URLs.
- Generic transport and routing code must not branch on provider or connection
  names. Workspace scoping is gated on the presence of
  `OpencodeAuthMetadata` — a static API key resolves to no extra headers.
- Protocol selection is declared, never inferred from a model name. An unknown
  `npm` package maps to no protocol.
- A token that the upstream surface expects in a header must not appear in a
  URL: query strings reach access logs and error reports.

---

## Alternatives considered

- **Ship the 401 as a user error (keep the zen route, document "use a key").**
  Rejected: the Console account issues no zen/go key, so the surface was
  unreachable by design, not by misconfiguration.
- **Retain `models.opencode.ai` as the catalogue and patch its wire
  baselines by hand.** Rejected: the two catalogs disagree on protocol
  (minimax), the public feed lags the account by 38 models, and every upstream
  addition would need a compiled edit.
- **Keep the zen-era `endpoint` derived from the compiled spec and override only
  the protocol.** Rejected: protocol and root are one decision upstream; the
  15 Anthropic and 7 Google models would have been sent to the OpenAI root.
- **Map a per-model full URL from `provider.api`.** Rejected: it forks the URL
  algebra into two composition paths and reintroduces the special-casing
  [ADR-0259](0259-deterministic-root-url-algebra-and-model-level-wire-protocol-inheritance.md)
  removed.
- **Alias `OpencodeGo` in serde and keep both shapes.** Rejected against the
  clean-break mandate: a stale shape name in a compiled enum invites a future
  contributor to route on it again.
- **Hard-code `x-opencode-org-id` for every opencode connection.** Rejected:
  keyless relays on the same origin have no workspace, and an empty header is a
  400 rather than an absent one; metadata presence is the only honest gate.
- **Reuse `ChatGptAuthMetadata` with the org id as `account_id`.** Rejected:
  [ADR-0268](0268-opencode-go-console-oauth-subscription.md) §4 exists precisely
  to stop a generic account id from projecting a connection onto the wrong
  surface's headers.

---

## Consequences

- **Positive.** Inference on `opencode-go` reaches the billing gate (402) or
  succeeds, instead of failing authentication (401). The account's own model
  list, wire protocol, and root drive every request.
- **Positive.** Adding a model, moving one to another root, or changing its npm
  package is now an upstream event that muta picks up on the next catalog sync;
  the curated baseline only has to cover the offline seed.
- **Positive.** Org scoping lives in the credential, so a later multi-workspace
  picker is an additive dimension on the catalog identity, not a new wire
  branch.
- **Negative.** The change is breaking-adjacent for hand-written configuration:
  a user-declared provider pinning `format = "opencode-go"` as its catalog
  shape now fails to load with a readable shape error and must be updated to
  `opencode-console`.
- **Negative.** Cached zen-era model ids keep routing on the new roots until the
  first Console fetch replaces them; per
  [ADR-0227](0227-connection-scoped-catalog-refresh-and-in-memory-source-cache.md)
  the cache never diminishes, so a stale id surfaces as an upstream 404 rather
  than a lost entry.
- **Neutral.** Funding is the user's, not the code's: an underfunded workspace
  still returns 402 after this change.

---

## References

- [ADR-0259](0259-deterministic-root-url-algebra-and-model-level-wire-protocol-inheritance.md) — root URL composition this ADR extends with an advertised root.
- [ADR-0265](0265-declarative-wire-surfaces.md) — dialect-declared wire surfaces.
- [ADR-0266](0266-declarative-remote-catalog-descriptors.md) — declarative remote catalog descriptors.
- [ADR-0267](0267-transport-middleware-pipeline-and-extensible-credential-architecture.md) — typed credential metadata, no provider-name branches.
- [ADR-0268](0268-opencode-go-console-oauth-subscription.md) — Console OAuth grant, partially superseded here.
- Implementation: `crates/muta-providers/src/registry/opencode_go.rs`, `crates/muta-providers/src/list_models.rs`, `crates/muta-agent/src/catalog/derive.rs`, `crates/muta-llm-client/src/endpoint.rs`.
