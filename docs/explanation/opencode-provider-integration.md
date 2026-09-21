# OpenCode Provider Integration

Working knowledge base for muta's OpenCode integration: the three OpenCode
service surfaces muta models, how each authenticates, which catalog and
inference roots it uses, how models are routed, and how to re-verify the
integration when upstream changes.

For the general subscription architecture, see
[OAuth2 subscription providers](oauth-subscription-providers.md). For the
provider-neutral rules this page applies, see
[ADR-0201](../adr/0201-model-provider-service-surface-and-connection-identity.md)
(provider identity vs authentication) and
[ADR-0267](../adr/0267-transport-middleware-pipeline-and-extensible-credential-architecture.md)
(no provider-name branches, typed credential metadata).

---

## 1. What OpenCode is and how muta classifies it

OpenCode (`opencode.ai`) exposes three distinct inference surfaces:

- **OpenCode Zen** — a pay-as-you-go AI gateway ("add credits, charged per
  request"). Acquired from the Console with an **API key**.
- **OpenCode Go** — a **$10/month subscription** for open coding models.
  Acquired from the Console with an **API key**.
- **OpenCode Console account** — the account/workspace-scoped inference
  surface reached by an interactive **OAuth device** sign-in.

These are not one surface with three switches. Per
[ADR-0201](../adr/0201-model-provider-service-surface-and-connection-identity.md)
a model provider is an `(endpoint family, wire dialect, model universe)` triple,
and authentication mode is a property of the connection, never of the provider
id. The Zen relay, the Go relay, and the Console account surface differ in
endpoint family and routing authority, so muta models them as **three model
providers, each with exactly one authentication mode**:

| Provider id | Surface | Auth | Inference root |
|---|---|---|---|
| `opencode` | Console account (workspace-scoped) | OAuth device (`ConnectionAuth::Subscription { provider: "opencode" }`) | `https://opencode.ai/inference/…` |
| `opencode-zen` | Zen relay | API key (`OPENCODE_API_KEY`) | `https://opencode.ai/zen/v1` |
| `opencode-go` | Go relay | API key (`OPENCODE_API_KEY`) | `https://opencode.ai/zen/go/v1` |

**Upstream differs deliberately.** OpenCode's own `opencode` integration
registers both an OAuth method and a key method and fetches
`GET /console/api/config` with either credential, then projects that one
credential onto every provider the response lists (including `opencode-go`).
muta instead keeps **one credential per connection** and lets the provider id
name the surface, because a provider whose endpoint depends on the connection's
auth mode would reintroduce the authentication-mode branching
[ADR-0267](../adr/0267-transport-middleware-pipeline-and-extensible-credential-architecture.md)
and
[ADR-0269](../adr/0269-opencode-console-catalog-and-routing-authority-with-workspace-scoping.md)
exist to remove.

---

## 2. Credentials, catalogs, and billing

`opencode` and `opencode-zen` both draw on the **same OpenCode account**, but
through different products and endpoints. `opencode-go` shares the account key
with `opencode-zen`; the endpoint decides which product is billed.

| | `opencode` | `opencode-zen` | `opencode-go` |
|---|---|---|---|
| Credential | Console OAuth access token (`st_…`) | Zen API key | Go API key |
| Credential metadata | `OpencodeAuthMetadata { org_id }` | none | none |
| Catalog | `GET /console/api/config` | `GET /zen/v1/models` | `GET /zen/go/v1/models` |
| Catalog auth | Bearer + `x-org-id` | public | public |
| Catalog content | account's models + per-model routing | ids only | ids only |
| Inference root | `/inference/{openai,anthropic,google}/…` | `/zen/v1` | `/zen/go/v1` |
| Workspace header | `x-opencode-org-id` on every inference call | none | none |
| Billing | Console account / workspace | Zen credits (per request) | Go subscription |

Verified against the public relays on 2026-09-21: both `/zen/v1/models` and
`/zen/go/v1/models` return `200` keyless, and the catalogues are **not** nested —
Zen exposed 74 ids and Go 37, with Go carrying ids Zen does not (for example
`qwen3.7-max`, `mimo-v2.5-pro`, `longcat-2.0`). Zen also exposed 8 zero-cost
`…-free` ids; Go exposed none.

The cross-surface isolation is the load-bearing fact
[ADR-0269](../adr/0269-opencode-console-catalog-and-routing-authority-with-workspace-scoping.md)
established by ablation:

| Surface | Console `st_…` bearer | `x-opencode-org-id` | Result |
|---|---|---|---|
| `/zen/go/v1/chat/completions` | sent | – | **401** Invalid API key |
| `/inference/openai/v1/chat/completions` | sent | – | **403** Workspace selection is required |
| `/inference/openai/v1/chat/completions` | sent | sent | **402** Insufficient funds (authenticated) |

So a Console credential never authenticates the Zen/Go relays, and the Console
inference surface is unusable without a workspace header. The credential that
authenticates a request must be the credential that selected the route.

---

## 3. Console OAuth device flow

OpenCode's device grant is **not RFC 8628**: it speaks JSON, returns a complete
`verification_uri_complete` (the user code is already embedded), and rotates the
refresh token from the same poll endpoint. It mirrors upstream
`packages/opencode/src/account/account.ts` and is implemented in
`crates/muta-providers/src/oauth/opencode_device.rs`, dispatched by
`DeviceFlowMode::Custom("opencode")`.

| Step | Request |
|---|---|
| Device code | `POST https://opencode.ai/console/auth/device/code` JSON `{"client_id":"opencode-cli"}` |
| Poll | `POST https://opencode.ai/console/auth/device/token` JSON `{"grant_type":"urn:ietf:params:oauth:grant-type:device_code","device_code":…,"client_id":"opencode-cli"}` |
| Refresh | same token endpoint, `{"grant_type":"refresh_token","refresh_token":…,"client_id":"opencode-cli"}` |
| Profile | `GET https://opencode.ai/console/api/user` (Bearer) |
| Orgs | `GET https://opencode.ai/console/api/orgs` (Bearer) |

The client id is `opencode-cli` (`oauth/presets.rs::OPENCODE_CLIENT_ID`); the
desktop app rewrites it to `opencode-desktop`, which muta does not emulate. The
poll honors `authorization_pending` (keep polling), `slow_down` (+5 s),
`access_denied`, and `expired_token`, with a 15-minute outer deadline.

`OpencodeOAuthEnricher` (`oauth/enricher.rs`) resolves account identity on login
and persists three attributes on the `TokenSet`:

- `account_id` — `GET /api/user` id; `user_email` from the same response.
- `org_id` / `org_name` — the first org after sorting by `(name, id)`, the same
  deterministic default upstream picks.
- `opencode_orgs` — the full membership, retained so a future org picker needs
  no second login.

On refresh the enricher copies the stored attributes forward, and — for a
credential minted before org resolution existed — refetches the org list once
so a missing `org_id` self-heals instead of stranding every Console request on
`Workspace selection is required`.

`OAuthCredentialSource::resolved` turns the stored `org_id` into the typed
`OpencodeAuthMetadata` extension, and only for the `opencode` subscription. The
projection is deliberately not derived from a generic `account_id`: a
non-OpenCode credential that happens to carry `org_id` must never be scoped onto
this surface (ADR-0268 §4).

---

## 4. Catalogs and routing

### 4.1 Console account catalog (`CatalogShape::OpencodeConsole`)

`opencode` discovers from `GET https://opencode.ai/console/api/config`
(`catalog_root_url` is the Console origin, declared separately from the
inference `root_url`). The response is
`{"config":{"provider":{"opencode":{"npm":…,"models":{…}}}}}` and is the
**routing authority**, not just a model list: each model may carry a
`provider:{npm,api}` override that selects the wire protocol and inference root.

`protocol_for_npm` maps the package to a protocol; an unknown package yields no
protocol and the model falls back to the spec default rather than a guess:

| `provider.npm` | `WireProtocol` | Root when `provider.api` overrides |
|---|---|---|
| `@ai-sdk/anthropic` | `AnthropicMessages` | advertised root, else `/inference/anthropic/v1` |
| `@ai-sdk/google` | `GoogleGemini` | advertised root, else `/inference/google/v1beta` |
| `@ai-sdk/openai` | `Responses` | default OpenAI root |
| `@ai-sdk/openai-compatible` | `ChatCompletions` | default OpenAI root |

The advertised `api` is an **API root**, never a full URL; the ADR-0259 algebra
appends the per-protocol suffix (`chat/completions`, `responses`, `messages`;
Google takes none). The compiled seed
(`registry/opencode.rs::MODELS`) only has to cover the offline list.

### 4.2 Zen and Go relay catalogs (`CatalogShape::OpenAi`)

`opencode-zen` and `opencode-go` discover from the public OpenAI-shaped
`/models` endpoints. The payload carries **ids only** — no protocol, no cost,
no routing override:

```json
{"object":"list","data":[{"id":"claude-sonnet-4-6","object":"model","created":1789952639,"owned_by":"opencode"}]}
```

Per-model routing therefore cannot come from the catalog. It comes from each
provider's compiled baseline table (`registry/opencode_zen.rs::MODELS`,
`registry/opencode_go.rs::MODELS`), and any discovered id that is not in that
table falls back to the spec default (`ChatCompletions`). This is the one place
where the key surfaces are structurally weaker than the Console surface, and it
is why the Zen baseline must cover every non-chat model family muta intends to
serve. See §7.

---

## 5. Wire details

- **Credential carrier follows the protocol, not the provider.** OpenAI
  Chat/Responses send the bearer; Anthropic uses `x-api-key`; Google, when the
  credential is org-scoped, sends `x-goog-api-key` and keeps the secret out of
  the URL. All four senders read the workspace header from typed metadata.
- **Two workspace spellings.** The catalog request sends `x-org-id`; inference
  sends `x-opencode-org-id`. The asymmetry is upstream contract, not a muta
  inconsistency, and both are presence-gated on `OpencodeAuthMetadata`, so a
  static API key emits neither.
- **Session affinity.** Any endpoint whose id starts with `opencode` or whose
  base URL contains `opencode.ai` attaches `x-opencode-session` (sticky routing
  / KV-cache reuse), `x-opencode-request` (trace UUID), and `x-opencode-client`
  when the caller is not already emulating the OpenCode client. This covers the
  Zen and Go relays by base URL, so `opencode-zen` inherits it with no new code.

---

## 6. Implementation map

| Concern | Location |
|---|---|
| Console provider preset + catalog parser | `muta-providers/src/registry/opencode.rs` |
| Zen provider preset | `muta-providers/src/registry/opencode_zen.rs` |
| Go provider preset | `muta-providers/src/registry/opencode_go.rs` |
| Provider id vocabulary + display metadata | `muta-contracts/src/model_providers.rs`, `muta-contracts/src/catalog.rs` |
| OAuth preset (device-only, JSON, no PKCE) | `muta-providers/src/oauth/presets.rs` (`opencode_preset`) |
| Device grant state machine | `muta-providers/src/oauth/opencode_device.rs` |
| Org/account enrichment + refresh healing | `muta-providers/src/oauth/enricher.rs` (`OpencodeOAuthEnricher`) |
| Workspace metadata projection | `muta-providers/src/oauth/credential_source.rs` (`OpencodeAuthMetadata`) |
| Workspace + affinity headers | `muta-llm-client/src/endpoint.rs` |
| Key env fallback (`OPENCODE_API_KEY`) | `muta-agent/src/catalog/derive.rs` |
| TUI templates | `apps/terminal/crates/mutx/src/providers.rs` |

---

## 7. Maintenance runbook

### 7.1 Re-verify the public relays (no credential needed)

```bash
curl -s https://opencode.ai/zen/v1/models | jq '.data | length'
curl -s https://opencode.ai/zen/go/v1/models | jq '.data | length'
```

A change in id set is expected; a change in the payload **shape** (for example
an added `provider` object) would let the key surfaces recover per-model routing
and should be turned into a parser change.

### 7.2 Triage by status code

| Symptom | Meaning |
|---|---|
| `401 Invalid API key` | A Console OAuth token sent to a Zen/Go relay (the relays reject Console credentials), or a key belonging to a different account. |
| `403 Workspace selection is required` | A Console request without `x-opencode-org-id`; the credential's `org_id` attribute is missing or empty. Re-login, or let the refresh enricher heal it. |
| `402 Insufficient funds` | Authenticated but unfunded. Billing state, not a code defect. |
| `404` on a known model | Stale catalog id after an upstream rename; refresh the connection catalog or update the compiled baseline. |

### 7.3 When the Console catalog drifts

Diff `config.provider.opencode.models` against
`registry/opencode.rs::MODELS`. A new `provider.npm` value that
`protocol_for_npm` does not know falls back to the spec default silently — add
the mapping if the package is real, otherwise leave it to the fallback.

### 7.4 When the relays add a non-chat model

The `/models` payload will not say which wire it speaks. Pin it in the
provider's compiled baseline (`OPENCODE_ZEN_MODELS` / `OPENCODE_GO_MODELS` seed
plus the `MODELS` table), using the upstream [Zen endpoint
table](https://opencode.ai/docs/zen) as the source of truth. A discovered id
without a baseline entry routes as chat completions and fails upstream.

---

## 8. Known-open items

1. **Zen baseline coverage.** `registry/opencode_zen.rs::MODELS` seeds three
   models. Claude/GPT/Gemini/Grok ids discovered from `/zen/v1/models` route as
   chat completions until pinned, so the baseline must be expanded (or the
   parser taught to read a future routing field) before the Zen key surface can
   serve the full catalogue correctly.
2. **No keyless free tier.** Zen exposes 8 `-free` ids that upstream serves with
   a literal `public` key and no account. muta has no keyless credential mode
   and no model cost metadata, so those models are unreachable on
   `opencode-zen`.
3. **OAuth cannot reach Zen/Go.** muta wires the Console credential only to the
   `opencode` provider. Upstream's one-credential-many-providers projection is
   intentionally not implemented; a user who wants the relays adds a key
   connection.
4. **`provider.api` root overrides are account-scoped.** They only arrive from
   the authenticated Console catalog; the public relays never publish them.

---

## 9. Design decisions worth remembering

1. **Three providers, never one provider with two auth modes.** Authentication
   must not select the route (ADR-0201 INV-1, ADR-0267, ADR-0269). The three
   surfaces differ in endpoint family and model universe, not merely in how the
   bearer is obtained.
2. **The account catalog beats the public mirror.** Discovery for an
   account-scoped surface must come from an endpoint that presents that
   account's credential; a public catalogue may lag and may disagree on wire
   protocol (ADR-0269).
3. **Workspace scoping is typed metadata, not a provider-name branch.** The
   header is emitted from `OpencodeAuthMetadata` presence, so static keys and
   keyless relays stay header-free with no special case.
4. **The relays share one env var.** `OPENCODE_API_KEY` authenticates both
   `opencode-zen` and `opencode-go`; the endpoint selects the product billed.
   A connection may still store its own key under its own name.

---

## References

- Upstream: `packages/core/src/plugin/provider/opencode.ts`,
  `packages/opencode/src/account/account.ts`, docs `zen.mdx` / `go.mdx`.
- ADRs: [0201](../adr/0201-model-provider-service-surface-and-connection-identity.md),
  [0267](../adr/0267-transport-middleware-pipeline-and-extensible-credential-architecture.md),
  [0268](../adr/0268-opencode-go-console-oauth-subscription.md),
  [0269](../adr/0269-opencode-console-catalog-and-routing-authority-with-workspace-scoping.md).
- [Provider reference](../reference/providers.md),
  [OAuth2 subscription providers](oauth-subscription-providers.md).
