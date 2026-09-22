# 0266. Declarative remote catalog descriptors: shapes, dimensions, and the retirement of `DiscoveryProtocol`

- **Status:** Accepted
- **Date:** 2026-09-20
- **Implementation:** `muta-contracts`, `muta-providers`, `muta-agent`, `muta-persistence`
- **Builds on:** [ADR-0203](0203-remote-catalog-overlay-and-connection-gated-pipeline.md), [ADR-0227](0227-connection-scoped-catalog-refresh-and-in-memory-source-cache.md), [ADR-0258](0258-first-class-declarative-model-providers-and-connection-pipe-purity.md)
- **Amends:** [ADR-0203](0203-remote-catalog-overlay-and-connection-gated-pipeline.md) §5 (`DiscoveryProtocol` → `CatalogShape`), [ADR-0259](0259-deterministic-root-url-algebra-and-model-level-wire-protocol-inheritance.md)
- **Related:** [ADR-0265](0265-declarative-wire-surfaces.md) (the surface that references this descriptor)

## Context

`RemoteCatalogSource::Endpoint(DiscoveryProtocol)` conflated two things: the
*shape* of a catalog response and the *provider* that serves it. `Codex`,
`OpencodeGo`, and `GoogleCloudCode` were each a distinct variant because each had
a distinct shape, so adding a provider whose shape already existed still meant a
new variant and a new match arm. The living blueprint already recorded this as
self-acknowledged debt: ADR-0203 renamed `discovery` to `RemoteCatalog`, but "the
code, wire types, and persisted filenames still say `discovery`".

Separately, nothing modeled the fact that one provider may serve **different
catalogs for different request dimensions**. Qoder's catalog is keyed by `scene`
(`assistant`, `experts`, …), and the two scenes expose different model sets — the
same account sees `qfmodel` (Qwen3.8-Flash) in `assistant` but not in `experts`.
A single cache keyed only by provider ignored this, so two dimensions would have
collided.

Finally, a catalog on a *signed* transport (Qoder's COSY SSE surface) needs to
authenticate with the dialect's own request signing, not a bearer token. There
was no way to declare that.

## Decision

A catalog is described by a **`CatalogShape`** and a small `CatalogSpec`, not by
a provider-specific enum.

### Shape is a closed set; the provider is not

`CatalogShape` is the closed set of response parsers: `OpenAi`, `Anthropic`,
`Google`, `GoogleCloudCode`, `Codex`, `OpencodeGo`, and `SceneMap` (Qoder's
`{scene: [entry]}` map). A new provider reuses an existing shape whenever it can
— the clean-break rename makes that explicit with no legacy alias, no dual
write, and no transitional shim.

### The `discovery` vocabulary is retired (ADR-0203 §1 delivered)

ADR-0203 §1 mandated replacing `discovery` with the catalog vocabulary and §29
mandated "zero transitional aliases, zero legacy shims". This ADR delivers that
rename in full:

| Retired | Canonical |
|---------|-----------|
| `DiscoveryProtocol` | `CatalogShape` |
| `DiscoveryCache`, `LockedDiscoveryCache` | `RemoteCatalogCache`, `LockedRemoteCatalogCache` |
| `DiscoveryOutcome` | `CatalogSyncOutcome` |
| `DiscoverySource` / `DiscoveryFetch` / `DiscoveryJob` | `CatalogFetchSource` / `CatalogFetchResult` / `CatalogSyncJob` |
| `DiscoveryTaskResult` | `CatalogSyncTaskResult` |
| `DiscoveryWarning` (wire tag `discovery_warning`) | `CatalogSyncWarning` (`catalog_sync_warning`) |
| `ModelDiscoveryRequest` / `Options` / `Update` | `RemoteCatalogRequest` / `Options` / `Update` |
| `discover_provider_models` / `discover_connection_models` | `sync_remote_catalog` / `sync_connection_catalog` |
| `models_discovery.json` (state) | `remote_catalog.json` |
| module `catalog::discovery` | module `catalog::sync` |

The word **stays** where ADR-0203 §2 says it is correct — service/peer
registration: the daemon record (`serve_discovery`, `global_discovery_path`),
mesh peer discovery, skill discovery, and file/tool discovery are untouched.

**Frozen historical facts are not vocabulary.** Retired on-disk *names* read by
one-shot user-data migration (`models_discovery.json`) remain as literals: the
`route_settings` fold rescues **user reasoning overrides** (non-derivable user
input) from a pre-rename file, so renaming the path would orphan the very data
the read exists to recover. The derivable catalog payload in that same file is
deliberately **not** migrated (ADR-0203 §29: no shims for regenerable state).


The shape carries the catalog's own facts: `path()`, `query()`, `auth()`,
`signed_path()`, and `dimensions()`. A reused shape therefore inherits its path
and authentication, so a new provider that shares a shape costs zero Rust code
— the complete form of ADR-0258 `[INV-PROV-02]`.

### Catalog authentication may be the dialect's signing

`CatalogAuth::Dialect` means "this catalog authenticates with the same signing
the dialect's inference uses". Qoder's `/model/list` is a COSY-signed GET whose
canonical form is identical to inference apart from the signed path
(`/api/v2/model/list` vs `/api/v2/service/pro/sse/agent_chat_generation`). One
signer implementation serves both; the shape declares `signed_path()`, and the
fetcher passes it to that one signer.

### Request dimensions are part of the catalog identity

`CatalogShape::dimensions()` declares the request dimensions that select *which*
catalog the server returns. These are folded into the discovery identity hash
alongside the base URL, protocol, client profile, and client version, so two
connections that differ only in a dimension (Qoder's `scene`) cache
independently and never share a validator or TTL.

A connection may **override** a dimension (`Connection::catalog_dimensions`,
keyed by dimension name). The provider's shape declares the defaults; the
connection selects a variant. This is a dimension of one catalog, not a second
catalog — the single-source invariant of ADR-0203 `[INV-CATALOG-02]` holds.

## Invariants

- A catalog is described by shape plus parameters; no provider-specific variant
  exists in core (ADR-0258 `[INV-PROV-02]`).
- A provider whose catalog shape already exists adds no Rust code.
- Catalog request dimensions are part of the discovery identity hash; two
  connections differing only by a dimension never share a cache entry.
- A dimension override selects a variant of one catalog, never a second catalog
  source for the connection (ADR-0203 `[INV-CATALOG-02]`).
- A live catalog is authoritative; a compiled-in seed covers only the window
  before the first refresh and a failed refresh (ADR-0227). The seed is not a
  frozen materialized list (ADR-0203 `[INV-CATALOG-05]`).
- Catalog capabilities come from the response fields, never from parsing the
  model id (ADR-0203 `[INV-CATALOG-01]`).

## Rejected alternatives and negative knowledge

- **Adding a `DiscoveryProtocol::Qoder` variant.** The per-provider enum is the
  debt this ADR removes; Qoder is `SceneMap`.
- **A second cache keyed by provider and scene.** The dimension is part of the
  discovery identity, not a second cache.
- **Treating a scene as a provider.** `scene=experts` is not a different
  provider; it is a dimension of the same signed catalog.
- **Deriving capability flags from the model key** (e.g. inferring vision from
  `is_vl` absent a field). Capabilities come from declared fields only.
- **Keeping `discovery` names in code, types, or persisted files.** The rename
  is a clean break; no alias, no dual write (ADR-0203's stated posture).
