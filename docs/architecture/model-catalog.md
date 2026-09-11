# Model catalog architecture

- Status: Living Blueprint
- Last Updated: 2026-09-11
- Scope: `muta-providers`, `muta-contracts`, `muta-persistence`, `muta-agent`, `muta-runtime`, `mutx`
- Governing records: [ADR-0203](../adr/0203-remote-catalog-overlay-and-connection-gated-pipeline.md),
  [ADR-0227](../adr/0227-connection-scoped-catalog-refresh-and-in-memory-source-cache.md),
  [ADR-0228](../adr/0228-direct-network-access-and-catalog-refresh-consistency.md)

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

Out of scope for this page: inference transport and wire-format selection per
route, prompt caching, token accounting, and effort ladder resolution. Those
read the catalog but do not define it; see
[Reasoning effort](../reference/effort.md) and
[Model metadata](../reference/model-metadata.md).

## 2. The three layers

| Layer | Origin | Lifetime | Authority |
|-------|--------|----------|-----------|
| **3. Compiled** | Provider registry tables in `muta-providers` | Shipped with the binary | Offline floor; always present for a preset |
| **2. Remote** | One pluggable network source per connection | In-memory plus `models_discovery.json` | Authoritative for membership when a source is configured |
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
`fitting: bool` field is gone. The only membership-level capability test is a
provider's explicit picker flag arriving from discovery.

## 5. The remote catalog (layer 2)

### 5.1 Source selection

A connection resolves to exactly one of these, in order:

| Effective source | When |
|------------------|------|
| Connection override: `Endpoint` | The connection pins a first-party catalog protocol |
| Connection override: `ModelsDev` | The connection reads a provider slice from models.dev |
| Provider default | The provider spec's `catalog_source` |
| `None` | No network sync; the compiled baseline is authoritative |

Overrides exist because the transport endpoint and the catalog source are
deliberately independent: a private relay can serve inference from its own base
URL while sourcing model metadata from a verified catalog entry.

### 5.2 models.dev

models.dev publishes one central document keyed by provider. The client holds
the fetched catalog in **process memory only** — there is no on-disk catalog
cache, no TTL schedule, and no background refresh loop. An explicit refresh
fetches once through a shared in-flight future and replaces the in-memory
document; every concurrent caller shares that single request, including its
failure. On a cold read the committed, pruned snapshot embedded at build time
is the offline floor.

Reading the catalog never fetches. This is what makes startup network-free.

### 5.3 First-party endpoints

A first-party source speaks the protocol the provider declares. Both OpenAI
inference protocols use the OpenAI `/models` shape; Anthropic, Google, Google
Cloud Code, Codex, and Copilot each have their own. Requests are conditional:
the stored validator is sent, and a `304` reuses the cached list.

Two admission rules apply while parsing, before anything is persisted:

- Provider-specific exclusions — non-chat capability types, entries with no
  text output modality, entries lacking the generate-content method, and
  provider-published deprecation lists.
- A provider's own picker flag. An explicit "not in the picker" excludes the
  model; `None` means "not stated" and admits it.

Parsed entries are sorted by id and de-duplicated, except for providers that
publish a meaningful priority order, which is preserved.

### 5.4 Request identity and validator scoping

An ETag is valid only for the complete request identity: source kind, endpoint,
discovery protocol, client-version string, and the emulated client identity
headers. The identity is hashed and stored beside the validator. When any
component changes, the stored validator is discarded and the next fetch is
unconditional. This prevents a catalog fetched for one client emulation profile
from being treated as current after the profile changes.

### 5.5 Orchestration and write policy

A refresh pass builds one job per discovery-capable connection and runs them
with bounded concurrency (eight in flight). A single models.dev refresh is
shared across the whole pass and started lazily by the first job that needs it,
so it overlaps with the first-party fetches.

Each result is applied as it completes, under a cross-process lock on the
discovery cache, so one slow provider never delays a fast one. A connection
deleted while its fetch was in flight is never resurrected by the response.

Per connection, one successful fetch writes:

- the admitted model ids, in discovery order;
- the advertised capability fields;
- fitted metadata for ids no compiled baseline knows;
- the validator, client version, source identity, and refresh timestamp.

A failed fetch writes nothing. Discovery warnings surface to the user as a
connection status rather than as a silent list change.

### 5.6 Persistence

Discovered state lives in `models_discovery.json` under the state directory,
not the cache directory: discovered ids, ETag revalidation state, and
advertised capability fields are program-generated state the user expects to
survive a restart rather than regenerable scratch data. Routes are derived from
these records at catalog-build time and are never persisted as channel tables.
See [Paths](../reference/paths.md) for the exact location and its legacy
adoption rule.

### 5.7 Refresh triggers

| Trigger | Scope | Notes |
|---------|-------|-------|
| User-initiated refresh in the model picker | All discovery-capable connections | Streams per-connection results back to the UI |
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
| ADR-0203 retires the word "discovery" | The code, the wire types, and the persisted file name still use it |

## 9. Historical lineage and founding ADRs

| Record | Contribution to this blueprint | Status |
|--------|-------------------------------|--------|
| [ADR-0002](../adr/0002-model-channel-abstraction.md) | Channels derived from a provider registry; one resolution source for startup and switching | Accepted |
| [ADR-0065](../adr/0065-runtime-fitted-model-capability-overlay.md) | The fitted capability overlay for ids no baseline knows | Superseded by ADR-0203 |
| [ADR-0070](../adr/0070-provider-scoped-remote-model-metadata.md) | Remote metadata scoped to one provider instance and model | Accepted |
| [ADR-0123](../adr/0123-provider-instances-as-state-derived-routes.md) | Connections carry no model-list state; routes are derived, never persisted | Accepted |
| [ADR-0149](../adr/0149-model-capability-resolution-order.md) | The capability precedence order this blueprint's axis B extends | Accepted |
| [ADR-0171](../adr/0171-three-layer-model-catalog-and-pluggable-network-sources.md) | The three-layer model and the pluggable network source | Proposed |
| [ADR-0198](../adr/0198-declared-models-on-preset-connections.md) | Declared models on preset connections | Superseded by ADR-0199 |
| [ADR-0199](../adr/0199-unified-cascading-model-resolution-architecture.md) | Unified model scope configuration and cascading resolution | Accepted |
| [ADR-0201](../adr/0201-model-provider-service-surface-and-connection-identity.md) | `ModelProvider` as the service surface, `Connection` as the named pipe | Accepted |
| [ADR-0203](../adr/0203-remote-catalog-overlay-and-connection-gated-pipeline.md) | Valve algebra, single-source catalogs, tristate merge, `[INV-CATALOG-*]` | Accepted |
| [ADR-0227](../adr/0227-connection-scoped-catalog-refresh-and-in-memory-source-cache.md) | In-memory models.dev source, per-connection refresh, no scheduled pass | Accepted |
| [ADR-0228](../adr/0228-direct-network-access-and-catalog-refresh-consistency.md) | Direct network access and shared-flight refresh consistency | Accepted |
| [ADR-0230](../adr/0230-three-valued-vision-and-declared-only-gating.md) | Three-valued vision gating behind the permissive floor | Accepted |
