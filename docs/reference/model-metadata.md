# Model Metadata

This page defines how muta determines a model's capability and route for one
provider channel. For provider availability and endpoints, see
[Providers](providers.md). For how the list those channels come from is
assembled in the first place — membership, refresh triggers, and the picker —
see [Model catalog architecture](../architecture/model-catalog.md). For the
decision history, see
[ADR-0070](../adr/0070-provider-scoped-remote-model-metadata.md).

## Sources

| Source | Scope | Used when | Authority |
|--------|-------|-----------|-----------|
| Per-provider baseline table | Model id | Every provider has no trusted value for a field | Baseline and offline fallback |
| Fitted overlay | Unknown model id | A trusted provider discovered an id not in any baseline table | Bare-id fallback outside a channel |
| Remote channel metadata | Connection and model | A trusted provider's live list explicitly supplies a field | Effective behavior for that channel |

Each provider's baseline table lives beside its other registry data (e.g.
`crates/muta-providers/src/registry/openai.rs`) and is submitted to
`muta_contracts`'s lookup machinery at link time via
`inventory::submit!(BaselineModels(...))`. `muta_contracts` owns only the
`resolve()` / `baseline_models()` mechanism — the data itself is distributed
per provider. See [`muta_contracts::model`](../../crates/muta-contracts/src/model.rs)
for the lookup precedence rules.

Remote metadata never changes another provider's channel. A model id can have
different routes or limits at different providers and accounts.

## Merge rules

The effective model capability view starts from the baseline table or its
conservative unknown-model fallback. Each non-empty remote field then replaces
the corresponding value.

| Field | Remote behavior |
|-------|-----------------|
| Family | Replaces the baseline family |
| Context window and output limit | Replaces the baseline value when present |
| Reasoning representation | Replaces the baseline thinking type when present |
| Tool calling and vision | An explicit `true` or `false` replaces the baseline |
| Effort levels | A present list replaces the baseline; an empty list disables effort control. See [Reasoning effort](effort.md) for the per-provider ladder and resolution chain |
| Endpoint | Selects the provider channel's inference surface only |

An omitted remote field is not a negative capability. It retains the static
baseline so partial provider responses do not erase useful local knowledge.

## Catalog discovery

Each connection uses one source selected by its provider's `RemoteCatalogSource`
or its `catalog_source` override:

| Source | Behavior |
|--------|----------|
| `Endpoint` | Reads the provider's catalog endpoint with the connection's credentials and client identity |
| `ModelsDev` | Reads that provider's slice from the shared models.dev document |
| `None` | Uses the compiled baseline without remote discovery |

Startup reads the persisted connection catalog and compiled baseline without
fetching. Explicit refresh and connection lifecycle events fetch the selected
source. There is no cross-source fallback or scheduled catalog refresh.
Overlapping models.dev refreshes share one request and its result, including
failures; a later explicit refresh starts another request. Reading the in-memory
catalog or compiled snapshot never initiates a fetch.

Directory requests use direct connections, platform certificate verification,
and one 10-second deadline covering connection establishment, response headers,
and the complete body. Timeout messages include the last observed transport
phase. Model streams use their own streaming timeout policy.

Each connection is persisted and published as it completes. Failed requests
retain its previous model list and metadata. A valid empty endpoint catalog
clears its discovered list. Source results still pass through provider and
connection filtering and user include/exclude rules before becoming routes.

## GitHub Copilot

The Copilot provider uses endpoint discovery. Its model list controls
the selectable set and each selectable model's route.

| Remote field | muta behavior |
|--------------|-----------------|
| `model_picker_enabled` | `false` excludes the model from the picker and channel set |
| `supported_endpoints` with `/chat/completions` | Uses the OpenAI Chat Completions adapter |
| `supported_endpoints` with `/responses` | Uses the OpenAI Responses adapter |
| `supported_endpoints` with `/v1/messages` | Uses the Anthropic Messages adapter with Copilot authentication |
| `capabilities.limits` | Supplies context and output limits |
| `capabilities.supports` | Supplies tools, vision, reasoning, and effort controls |

Copilot discovery sends the OAuth bearer and Copilot client identity headers.
The response therefore reflects the logged-in account's entitlements rather
than a generic static plan assumption.

## Persistence

Discovery stores model ids, fitted capabilities, remote metadata, and ETag
validation state per connection in
`$XDG_STATE_HOME/muta/models_discovery.json`. Successful discovery replaces that
connection's records; failures preserve them. Routes are derived from these
records and the connection configuration rather than persisted as channel tables.

The raw models.dev document exists only in daemon memory. The committed snapshot
is the offline floor; `$XDG_CACHE_HOME/muta/models-dev.json` is not read or written.
See [Paths](paths.md) for legacy file locations.
