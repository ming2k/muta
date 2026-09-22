# Model catalog architecture

- Status: Living Blueprint
- Last Updated: 2026-09-21
- Scope: `muta-providers`, `muta-contracts`, `muta-persistence`, `muta-agent`, `muta-runtime`, `mutx`
- Governing records: [ADR-0203](../adr/0203-remote-catalog-overlay-and-connection-gated-pipeline.md),
  [ADR-0227](../adr/0227-connection-scoped-catalog-refresh-and-in-memory-source-cache.md),
  [ADR-0228](../adr/0228-direct-network-access-and-catalog-refresh-consistency.md),
  [ADR-0266](../adr/0266-declarative-remote-catalog-descriptors.md)
- Open records affecting this blueprint: none ([ADR-0273](../adr/0273-upstream-availability-as-a-fourth-catalog-axis.md) delivered the fourth axis)

---

## 1. System overview and boundaries

The model catalog answers three questions about a connection. They are
independent axes, resolved by different code, and conflating them is the most
common source of confusion:

| Axis | Question | Owner |
|------|----------|-------|
| **A. Membership** | Which model ids does this connection serve? | Set algebra over connection and provider scope |
| **B. Capability** | What can each of those models do? | Capability cascade plus remote metadata |
| **C. Presentation** | What does the picker list, in what order? | Daemon projection plus client sectioning |

Only axis B is a genuine field-level overlay. Axis A is a single-source
selection followed by a valve equation, never a merge of the layers. Axis C is
a projection with its own filters, and it is the only axis where the keyword
`key_ready` and the user's `hidden_models` have any effect.

A fourth question — *may this account run this model right now, and why not* — is
a **fourth axis** ([ADR-0273](../adr/0273-upstream-availability-as-a-fourth-catalog-axis.md)),
resolved by its own code and enforced in one place:

| Axis | Question | Owner |
|------|----------|-------|
| **D. Availability** | May this account run this model, and — when the provider says — why not? | `derive::effective_availability` (verdict), `build_provider_for_model` (enforcement) |

It carries **two independent declared predicates**, never ANDed: `availability`
(tristate verdict + the provider's own reason, verbatim) and `advertised` (the
provider's listing intent). Both are declarations about an *(account, model)*
pair; neither touches membership or capability. An unavailable model is a full
member that keeps its capabilities and its derived route shape, is still listed so
the account can see what an entitlement change would unlock, and is refused by the
daemon rather than by a client. See §4 and §5.3 for the details, §7 for the
`[INV-AVAIL-*]` invariants, and §5.5 for the retained-verdict staleness rule.

Out of scope for this page: inference transport and wire-format selection per
route, prompt caching, token accounting, and effort ladder resolution. Those
read the catalog but do not define it; see
[Reasoning effort](../reference/effort.md) and
[Model metadata](../reference/model-metadata.md).

### Route derivation boundary

[ADR-0260](../adr/0260-provider-dialect-inheritance.md) separates model protocol
selection from provider dialect inheritance. A remote model protocol overrides
the provider-scoped baseline and default. Provider dialect, endpoint selection,
and credential acquisition remain separate inputs. The final model protocol
selects any provider-declared protocol endpoint before constructing the adapter.

For Antigravity, remote models can omit their protocol and inherit Google Gemini;
the provider's Antigravity dialect supplies the internal request envelope and
path. Credential type does not affect that choice. Invalid protocol/dialect
combinations produce route errors; catalog construction retains the other valid
channels. Global model identity does not determine a provider's wire route.

## 2. The three layers

| Layer | Origin | Lifetime | Authority |
|-------|--------|----------|-----------|
| **3. Compiled** | Provider registry tables in `muta-providers` | Shipped with the binary | Offline floor; always present for a preset |
| **2. Remote** | One pluggable network source per connection | In-memory plus `remote_catalog.json` | Authoritative for membership when a source is configured |
| **1. User** | `connections.toml`, `model_providers.toml`, favorites, route settings | User-owned, persisted | Sovereign; wins every tie |

Layer 3 lives beside each provider as two tables: a `MODELS` baseline (the
capability truth for an id) and an offering list (the curated, ordered ids the
preset seeds). The baselines are submitted into the contracts crate's lookup
machinery at link time via `inventory::submit!(BaselineModels(..))`, so
`muta_contracts` owns only the `resolve()` / `baseline_models()` mechanism while
the data stays distributed per provider.

Layer 2 is a *single* source per connection, chosen by the provider spec's
`catalog_source` or a connection-level override. Multi-source fallback
waterfalls are prohibited (`[INV-CATALOG-02]`).

## 3. Membership — which models exist (axis A)

### 3.1 Layer selection is a branch, not a merge

Membership resolution picks exactly one layer, then filters it:

```text
candidates(c) = remote_catalog(c)      when the connection has a catalog source
              | provider_seed(c)       otherwise
```

A connection whose provider declares a catalog source takes the persisted
remote list verbatim when that list is present — **including when it is empty**
— and falls back to the compiled seed only when no remote list exists at all.
A provider with no catalog source always uses its compiled seed.

A structurally valid empty remote catalog therefore means "this connection
currently serves nothing", and it clears the connection's list. Transport,
authorization, HTTP-status, and schema failures are not valid empty catalogs:
they preserve the previous list instead (`[INV-CATALOG-03]`).

### 3.2 The connection valve

The selected candidates then pass through a deterministic valve:

```text
output(c) = ( filter(candidates, c) ∪ inject(provider) ∪ inject(c) )
            \ block(provider) \ block(c)
```

Applied in this exact order:

1. **Filter** — admission policy, defaulting per provider type (see below).
2. **Provider inject / block** — the provider-wide delta from
   `model_providers.toml`.
3. **Connection inject** — sovereign; bypasses the filter unconditionally
   (`[INV-CATALOG-04]`).
4. **Connection block** — prunes unconditionally, including injected ids.

Deduplication is membership-based and order-preserving: seeded or discovered
order first, then provider injections, then connection injections.

### 3.3 Filter policy defaults

| Connection | Default filter | Effect |
|------------|----------------|--------|
| `provider = "custom"` | `all` | Any id the relay advertises is admitted |
| Provider declares a remote catalog source | `all` | The upstream list replaces the baseline without intersection |
| Provider is fixed (no remote source) | `baseline` | Only ids present in the compiled baseline are admitted |

A connection may override the policy with `baseline`, `all`, or a glob list.
The choice is persisted as a *rule*, never as a frozen list of materialized ids
(`[INV-CATALOG-05]`).

`baseline` is an intersection against the compiled baseline table. It is the
strict, safe admission mode, and it is opt-in rather than the default for any
provider that publishes a remote catalog.

### 3.4 Where the user's intent lives

| Surface | File or store | Scope |
|---------|---------------|-------|
| `models.filter` / `models.inject` / `models.block` / `models.overrides` | `connections.toml` | One connection |
| `inject` / `block` / `overrides` | `model_providers.toml` | Every connection to one provider |
| `favorites` | `config.toml` | One model id, wherever it is served |
| `hidden_models` | `config.toml` | Presentation only (axis C) |
| route settings | `route_settings` state store | One connection and model |

The loader accepts the legacy aliases `include` and `exclude` for `inject` and
`block`; `inject` and `block` are canonical going forward.

## 4. Capability — what a model can do (axis B)

Capability resolution *is* a layered cascade, with a fixed precedence:

```text
route settings overrides
  > connection overrides
    > provider overrides
      > connection-declared facts
        > provider-declared facts
          > remote advertised metadata
            > compiled baseline
              > conservative floor
```

Two properties make this safe against partial upstream responses:

- **Tristate preservation.** A capability patch distinguishes "field omitted"
  from "field explicitly false". An omitted field falls through to the layer
  below; an explicit `false` is a declaration and wins
  (`[INV-CATALOG-06]`).
- **No heuristic synthesis.** Capability values are never inferred by pattern
  matching a model id. An id nobody declares degrades to a conservative floor
  rather than a guessed profile (`[INV-CATALOG-01]`).

The floor for an unknown id is a 128k context window, `tool_call: false`, and
`vision: true`. The permissive vision default is a routing *policy* — attempt
the request with images rather than silently strip them — not a capability
claim; whether anything was actually declared is a separate question answered
by `declared_vision()`.

Capability resolution never changes membership. There is no longer any pass
that removes a model because its capabilities look wrong: the former
`fitting: bool` field is gone. Availability is equally orthogonal — a model the
provider declared unusable stays a member and keeps every advertised capability;
only its *routability* changes, and that change is enforced daemon-side (§4, §7).

### 4a. Availability: the fourth axis

`RemoteModelMetadata` carries two declared predicates, and `DiscoveredModel`
mirrors them:

```rust
pub availability: Option<Availability>,   // { usable: bool, reason: Option<String> }
pub advertised: Option<bool>,             // the provider's listing intent
```

- `availability` is the verdict. `None` means the provider declared nothing and
  the model is usable; `Some(usable: false)` is a declaration that this account
  may not run it. `reason` is the provider's own explanation, **verbatim** —
  rendered or omitted, never parsed and never invented. Qoder states only
  `enable`, so its reason is legitimately `None`; the surfaces then say only that
  the model is unavailable rather than guessing "your plan".
- `advertised` is *listing*, deliberately separate. Codex's `visibility` says
  whether a model belongs in a listing; `supported_in_api` says whether the API
  will serve it. Its own fixture (`hidden-helper`: hidden **and** API-supported) is
  why these must never be ANDed into one bit (`[INV-AVAIL-07]`).
- **Enforcement is daemon-side and singular.** `derive::effective_availability`
  resolves the declaration against the connection's own scope and
  `build_provider_for_model` refuses a verdict that says no. Every session start,
  model switch, and bootstrap passes through it, so no client is the only refusal
  site (`[INV-AVAIL-06]`).
- **Sovereignty overrides and discloses.** An explicit `inject`/`include` makes an
  unavailable model usable (`[INV-CATALOG-04]`), the upstream declaration is never
  rewritten, and the projection reports `availability_overridden` so the row says
  `locked upstream · overridden by you` (`[INV-AVAIL-05]`).
- **Listing is a hint, not a gate.** `advertised: Some(false)` marks a model the
  provider does not want offered by default; it does not make the model unusable.

## 5. The remote catalog (layer 2)

### 5.1 Source selection

A connection resolves to exactly one of these, in order:

| Effective source | When |
|------------------|------|
| Connection override: `Endpoint` | The connection pins a first-party catalog protocol |
| Provider default | The provider spec's `catalog_source` |
| `None` | No network sync; the compiled baseline is authoritative |

Overrides exist because the transport endpoint and the catalog source are
deliberately independent: a private relay can serve inference from its own base
URL while sourcing model metadata from a verified catalog entry.

### 5.2 Retired sources

There is no third-party Layer-2 source. The former models.dev feed
(`LiveCatalog::ModelsDev`, the standalone `muta-models-dev` module, and its
committed build-time snapshot) was removed: `catalog_source` has exactly two
arms, `Endpoint(CatalogShape)` and `None` (§5.1), and no code path fetches the
models.dev document. A provider whose catalog lives there is now reached through
its own first-party `Endpoint`.

Consequently there is no central models.dev document, no in-memory catalog
cache, and no embedded snapshot; the offline floor is the compiled baseline
(§2, Layer 3). Reading a catalog never fetches, so startup stays network-free
(§5.7).

### 5.3 First-party endpoints

A first-party source speaks the protocol the provider declares. Both OpenAI
inference protocols use the OpenAI `/models` shape; Anthropic, Google, Google
Cloud Code, Codex, and Copilot each have their own. Requests are conditional:
the stored validator is sent, and a `304` reuses the cached list.

One exclusion rule applies while parsing, before anything is persisted:

- Provider-specific exclusions — non-chat capability types, entries with no
  text output modality, entries lacking the generate-content method, and
  provider-published deprecation lists. A fully retired model is *gone*, which is
  membership removal, not an availability verdict.

A provider's availability declaration is **not** an admission rule and excludes
nothing at parse time. Each shape maps only the fields its own vendor publishes
(ADR-0273 §2):

| Shape | Vendor field | Mapped to |
|-------|--------------|-----------|
| Qoder | `enable` | `availability` (no reason field exists → `reason: None`) |
| Codex | `supported_in_api` | `availability` |
| Codex | `visibility == "list"` | `advertised` |
| Copilot | `policy.state` | `availability` — only an explicit `disabled` is a declaration; `unconfigured`, `unknown`, and an absent policy are **undeclared** |
| Copilot | `policy.terms` | `reason`, verbatim |
| Copilot | `model_picker_enabled` | `advertised` |

The declarations are persisted with the entry (`None` = "not stated" = usable and
listed, never rendered as disabled), reach the picker as the dim/reason, are
enforced by the daemon (§4a), and withhold the fitted registration unless the
connection's own scope injected the model.

Parsed entries are sorted by id and de-duplicated, except for providers that
publish a meaningful priority order, which is preserved.

### 5.4 Request identity and validator scoping

An ETag is valid only for the complete request identity: source kind, endpoint,
catalog shape, client-version string, the emulated client identity headers, and
the resolved catalog dimensions (ADR-0266). The identity is hashed and stored
beside the validator. When any component changes, the stored validator is
discarded and the next fetch is unconditional. This prevents a catalog fetched
for one client emulation profile — or one scene — from being treated as current
after the profile or dimension changes.

### 5.5 Orchestration and write policy

A refresh pass builds one job per catalog-capable connection and runs them
with bounded concurrency (eight in flight).

Each result is applied as it completes, under a cross-process lock on the
catalog cache, so one slow provider never delays a fast one. A connection
deleted while its fetch was in flight is never resurrected by the response.

Per connection, one successful fetch writes:

- the admitted model ids, in catalog order;
- the advertised capability fields;
- the two availability declarations (`availability`, `advertised`) alongside the
  rest of the advertised metadata;
- fitted metadata for ids no compiled baseline knows;
- the validator, client version, source identity, refresh timestamp, and
  `refresh_failed: false`.

A failed fetch writes nothing to the model data — the previous list, metadata, and
verdicts are retained (`[INV-CATALOG-03]`) — but it does set `refresh_failed` on
the connection's state, so the retained availability verdicts present as
*as last observed* rather than freshly confirmed (`[INV-AVAIL-08]`). Enforcement is
unchanged by the flag: a failure never widens access.

Catalog warnings surface to the user as a connection status rather than as a silent
list change, and that status distinguishes a durable upstream **refusal** (the
upstream answered `401`/`403`) from a transient failure
(`ConnectStatus::CatalogSyncWarning { kind }`, `[INV-AVAIL-09]`). The typed fetch
error is carried intact to that point; it is no longer stringified at the fetch
boundary.

### 5.6 Persistence

Discovered state lives in `remote_catalog.json` under the state directory,
not the cache directory: discovered ids, ETag revalidation state, and
advertised capability fields are program-generated state the user expects to
survive a restart rather than regenerable scratch data. Routes are derived from
these records at catalog-build time and are never persisted as channel tables.
See [Paths](../reference/paths.md) for the exact location and its legacy
adoption rule.

### 5.7 Refresh triggers

| Trigger | Scope | Notes |
|---------|-------|-------|
| User-initiated refresh in the model picker | All catalog-capable connections | Streams per-connection results back to the UI |
| Connection added or connected after authentication | That one connection | Login and add flows never wait on unrelated providers |
| An in-flight model response advertising a new catalog validator | That one connection | Event-initiated, never a scheduled poll |

The third trigger is rate-limited to half the freshness interval. When the
advertised validator matches the stored one and that interval has not elapsed,
nothing happens at all; when it matches but the interval has passed, the
response only renews the freshness stamp and no network request is made; when it
differs, a full refresh for that connection runs and the catalog change is
broadcast to clients.

**Startup is read-only.** The process loads the persisted catalog and the
compiled baseline without contacting the network, and no schedule exists that
would fetch later on its own.

## 6. Presentation — what the user sees (axis C)

### 6.1 Daemon projection

The daemon derives the picker state from the stores on every request rather
than caching a UI list. For each connection it emits the served model ids, the
active model, key readiness, and per-model information.

Two presentation-only filters apply here:

- `hidden_models` from configuration removes matching models from the picker.
  If every model of a connection is hidden, the projection falls back to
  showing all of them: the list cannot be emptied this way.
- Favorite state is model-level and recency is (connection, model)-level. Both
  travel as data so every frontend renders the same sections from the same
  numbers.

Hidden models remain routable. `hidden_models` is a presentation filter, not an
admission rule; it never enters the valve equation.

### 6.2 The wire type

The picker payload is a snapshot: a default connection id plus one row per
known connection, each row carrying its model ids and per-model information.
It travels as an agent response and is also embedded in the session snapshot.
Every mutating provider action republishes the whole snapshot rather than an
incremental patch, so a client never merges its own view.

There is no REST model-listing endpoint and no CLI model-listing verb. A
consumer that needs a model list attaches to the daemon and speaks the control
protocol. See the [Server API reference](../reference/server-api.md).

The snapshot is the frontend contract for both the TUI and the web client;
`key_ready` is the only readiness signal a client needs, and readiness never
implies the model is entitled — a later request may still be refused upstream.

### 6.3 Client-side list construction

The client builds the visible list from the snapshot in one pass:

1. **Readiness gate.** Rows for connections without a usable credential are
   dropped entirely, which is why a connection can exist in the Connections
   list but contribute no rows to the model list.
2. **Flatten.** One row per (connection, model) pair.
3. **Label.** The provider's own published name when it advertises one,
   otherwise the raw wire id. The label is never curated client-side, so there
   is no name table to drift.
4. **Filter.** A fuzzy match over the rendered label, the wire id behind it,
   and the provider name, applied only while a query is present.
5. **Section.** Favorites, then recency, then all models — see below.

The wire id always remains the identity: activation, favoriting, hiding, and
every configuration surface key on it, and a name-first row keeps the id
visible beside the label.

### 6.4 Sectioning and ordering

| Section | Membership | Order |
|---------|------------|-------|
| Favorites | Models the user starred | Wire id, then provider label |
| Recent | Models with activation history on that connection | Most recently used first, then wire id, then provider label |
| All models | Every pair from a ready connection | Wire id, then provider label |

Precedence is favorites, then recent, then everything else. A favorite is
deliberate user intent and outranks the emergent recency signal. The all-models
section repeats the favorite and recent rows rather than excluding them, so it
is always a complete listing. An empty section renders no heading at all.

The all-models section orders by wire id, not by the rendered label. The list
is stable under a provider renaming a model and stays consistent with the
identity every other surface uses.

The **currently active pair is not pinned** to the top. It keeps its natural
section position and is marked by a glyph, and the picker opens with the
selection on it.

### 6.5 Picker verbs

| Key | Effect |
|-----|--------|
| `↑` / `↓` | Move the selection |
| `/` | Enter search mode; a query filters the list |
| `Enter` | Activate the highlighted pair |
| `*` | Toggle favorite on the highlighted model |
| `x` | Block the highlighted model on its connection |
| `e` | Open the per-model editor (effort and thinking) |
| `r` | Refresh the connection catalogs |
| `Esc` | Close |

Blocking writes a connection-scope valve rule; activating writes the active
model. Both are ordinary catalog mutations and come back as a fresh snapshot.

### 6.6 The model bar

The status bar renders the **wire id** of the active model, not the picker
label, together with its context window. It also verifies membership: if the
snapshot no longer lists the active model on its connection, the bar marks it
unavailable rather than silently keeping a stale name.

## 7. Active invariants

| Id | Rule | Enforced by |
|----|------|-------------|
| `[INV-CATALOG-01]` | Capability values are never synthesized from model-id patterns | Conservative floor instead of prefix heuristics |
| `[INV-CATALOG-02]` | Exactly one remote catalog source per provider or connection | Source selection returns one variant or nothing |
| `[INV-CATALOG-03]` | Network failure never evicts or diminishes the stored catalog | A failed apply writes nothing |
| `[INV-CATALOG-04]` | Connection injections bypass the connection filter unconditionally | Valve step order |
| `[INV-CATALOG-05]` | Persistence stores filter rules, never materialized id lists | `filter` is a policy value in the connection record |
| `[INV-CATALOG-06]` | A missing capability field and an explicit false are distinct | Tristate patch deserialization |
| `[INV-CATALOG-07]` | An omitted client profile inherits the provider's recommended profile | Provider spec carries the recommendation and a sensitivity flag |
| `[INV-AVAIL-01]` | Membership, capability, presentation, and availability are four owners; none is implemented by mutating another | Separate resolution paths; availability never edits the id set or capabilities |
| `[INV-AVAIL-02]` | A verdict or reason is recorded only when the provider declares it — never derived from ids, status codes, or error text | Per-shape field mapping only |
| `[INV-AVAIL-03]` | The reason is inert display data: never parsed, matched, or decided on | `reason` is read only by presentation |
| `[INV-AVAIL-04]` | Only the tristate verdict gates; undeclared is never rendered disabled | `Availability` + client projections |
| `[INV-AVAIL-05]` | A user scope may override a verdict, never rewrite it, and the override is disclosed | `effective_availability` + `availability_overridden` |
| `[INV-AVAIL-06]` | No client is the only refusal site: a declared-unavailable route is not built daemon-side | `build_provider_for_model` gate |
| `[INV-AVAIL-07]` | Listing intent and availability are independent and never ANDed | Separate `advertised` / `availability` fields |
| `[INV-AVAIL-08]` | A verdict retained across a failed refresh is marked stale; enforcement never depends on the mark | `ModelListCacheState::refresh_failed` → `availability_stale` |
| `[INV-AVAIL-09]` | A refused connection is a connection-level fact, distinct from a transient failure | `CatalogSyncWarning { kind }` + `ModelListError::is_refusal` |

## 8. Deltas from the decision records

The living behavior differs from the records below in ways that are deliberate
or known-stale. They are listed so a reader does not trust a superseded
sentence over the running code.

| Record claim | Behavior today |
|--------------|----------------|
| ADR-0203 §3 describes a `ProviderMasterCatalog = Baseline ⊕ RemoteCatalog` type | No such type exists; membership is a single-source branch, and only capability fields overlay |
| ADR-0199 describes a default `Baseline ∩ Discovery` intersection | Open admission (`all`) is the default for custom providers and for any provider with a remote catalog source; intersection is opt-in |
| ADR-0203 §4 says the unknown-model floor is text-only with vision disabled | The floor disables tool calling and keeps the permissive vision routing policy (ADR-0230) |
| ADR-0203 §10 describes a four-tier sort with curated baseline order and a pinned active pair | Presentation is three sections ordered by wire id or recency; the active pair is not pinned |
| ADR-0203 retires the word "discovery" | **Resolved.** ADR-0266 delivered the full rename: `CatalogShape` (was `DiscoveryProtocol`), `RemoteCatalogCache`, `CatalogSyncOutcome`, `CatalogSyncWarning`, `sync_remote_catalog`, the `catalog::sync` module, and the `remote_catalog.json` state file. The word remains only where ADR-0203 §2 says it is correct — service/peer/skill/tool registration — and as frozen on-disk names read by one-shot user-data migration |
| ADR-0268 kept the compiled OpenCode Go routes and the keyless `models.opencode.ai` catalogue, deferring `/api/config` | **Resolved.** ADR-0269 made the authenticated Console `api/config` the authority for OpenCode Go: it declares each model's protocol and API root, which `RemoteModelMetadata.endpoint` carries into the ADR-0259 route algebra |
| §4 said a provider's picker flag was the only *membership-level capability* test | **Resolved.** Nothing gates on a picker flag: availability is its own axis (§4a) enforced daemon-side, and the flag itself (`picker_enabled`) is retired in favour of `availability` + `advertised` ([ADR-0273](../adr/0273-upstream-availability-as-a-fourth-catalog-axis.md)) |
| §5.3 and [Model metadata](../reference/model-metadata.md) both said a provider's picker flag *excludes* the model from the picker and channel set | **Corrected in place.** No shape excludes on an availability declaration: membership is untouched, and only Google Cloud Code's `deprecatedModelIds` truly prunes (retirement = membership; unavailable = axis D) |
| §6.2 said `key_ready` is the only readiness signal a client needs | **Resolved.** `availability` and `availability_overridden` join it, and `availability_stale` records that a retained verdict survived a failed refresh rather than implying it is current |

## 8a. Catalog shapes and dimensions (ADR-0266)

A catalog is described by a `CatalogShape` (a closed set of response parsers —
`OpenAi`, `Anthropic`, `Google`, `GoogleCloudCode`, `Codex`, `OpencodeConsole`,
`SceneMap`), not a provider-specific variant. The shape carries its own
`path()`, `query()`, `auth()`, `signed_path()`, and `dimensions()`, so a provider
that reuses a shape inherits all of them with no new Rust code.

| Shape | Path | Auth | Dimensions |
|-------|------|------|------------|
| `OpenAi` (default) | `models` | bearer | — |
| `OpencodeConsole` (OpenCode Go) | `api/config` | bearer + `x-org-id` | — |
| `SceneMap` (Qoder) | `algo/api/v2/model/list` | dialect signature | `scene` (`assistant`) |

`CatalogAuth::Dialect` means the catalog authenticates with the dialect's own
inference signing; one signer serves both the inference path and the catalog
path. The shape's `signed_path()` is the only difference between the two
canonical forms.

**Dimensions select a variant of one catalog.** `CatalogShape::dimensions()`
declares them; `Connection::catalog_dimensions` overrides them by name; the
resolved set is folded into the catalog identity hash, so `scene=assistant`
and `scene=experts` cache independently. This is not a second catalog source —
`[INV-CATALOG-02]` holds (see ADR-0266).

## 8b. Wire surfaces (ADR-0265)

A dialect key resolves to a `DialectSurface` (declared in `muta-contracts`):
its inference path and query, its identity (emulated version, version header,
static headers), its request envelope, and its model-identity carriers.

| Concern | Single home | Readers |
|---------|-------------|---------|
| Emulated client version | `IdentitySpec.emulated_version` | the `version_header`, the signature payload, the envelope `business.version` |
| Identity headers | `IdentitySpec::{headers, version_header}` via `headers_with_version()` | the inference executor and the catalog signer — **both** call `headers_with_version()` |
| Model identity slots | `InferenceSpec.model_bindings` | `qoder_envelope::apply_model_bindings` — the **one** reader, for every carrier kind |
| Request envelope | `InferenceSpec.envelope` (`Flat` \| `AgentChat`) | the request builder |

The model-identity value is always the channel's wire id verbatim; a carrier
declares *where* it appears, never *what* it is (ADR-0131). No request-composition
path branches on a provider name beyond resolving the dialect key to its surface
(ADR-0260).

**Three rules that keep this true** (each had a drift defect during
implementation, now guarded by tests):

1. Every reader of the identity headers calls `headers_with_version()`, never
   the raw `headers` table — the version header is not optional.
2. No path writes a model-identity slot directly; `apply_model_bindings` is the
   only writer, so a `model_config.key` or `X-Model-Key` cannot be set behind
   the declaration's back.
3. A dialect has exactly one header-declaration site (its surface). The generic
   chat-completions header table (`request::headers`) carries **no** Qoder
   branch, because the executor never routes Qoder through it.


## 9. Historical lineage and founding ADRs

| Record | Contribution to this blueprint | Status |
|--------|-------------------------------|--------|
| [ADR-0002](../adr/0002-model-channel-abstraction.md) | Channels derived from a provider registry; one resolution source for startup and switching | Accepted |
| [ADR-0065](../adr/0065-runtime-fitted-model-capability-overlay.md) | The fitted capability overlay for ids no baseline knows | Superseded by ADR-0203 |
| [ADR-0070](../adr/0070-provider-scoped-remote-model-metadata.md) | Remote metadata scoped to one provider instance and model | Accepted |
| [ADR-0123](../adr/0123-provider-instances-as-state-derived-routes.md) | Connections carry no model-list state; routes are derived, never persisted | Accepted |
| [ADR-0149](../adr/0149-model-capability-resolution-order.md) | The capability precedence order this blueprint's axis B extends | Accepted |
| [ADR-0171](../adr/archive/0171-three-layer-model-catalog-and-pluggable-network-sources.md) | The three-layer model and the pluggable network source (now this blueprint's binding rule) | Compacted |
| [ADR-0198](../adr/0198-declared-models-on-preset-connections.md) | Declared models on preset connections | Superseded by ADR-0199 |
| [ADR-0199](../adr/0199-unified-cascading-model-resolution-architecture.md) | Unified model scope configuration and cascading resolution | Accepted |
| [ADR-0201](../adr/0201-model-provider-service-surface-and-connection-identity.md) | `ModelProvider` as the service surface, `Connection` as the named pipe | Accepted |
| [ADR-0203](../adr/0203-remote-catalog-overlay-and-connection-gated-pipeline.md) | Valve algebra, single-source catalogs, tristate merge, `[INV-CATALOG-*]` | Accepted |
| [ADR-0227](../adr/0227-connection-scoped-catalog-refresh-and-in-memory-source-cache.md) | In-memory models.dev source, per-connection refresh, no scheduled pass | Accepted |
| [ADR-0228](../adr/0228-direct-network-access-and-catalog-refresh-consistency.md) | Direct network access and shared-flight refresh consistency | Accepted |
| [ADR-0230](../adr/0230-three-valued-vision-and-declared-only-gating.md) | Three-valued vision gating behind the permissive floor | Accepted |
