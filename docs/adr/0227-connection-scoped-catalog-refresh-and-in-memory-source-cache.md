# 0227. Catalog refresh is connection-scoped and user-initiated; the raw source catalog is an in-memory cache

- **Status:** Accepted
- **Date:** 2026-09-10
- **Amends:** [ADR-0171](0171-three-layer-model-catalog-and-pluggable-network-sources.md) — retires the on-disk
  `models.dev` catalog cache, its TTL/lock, and the hourly `DynamicModelsDev` background refresh; and collapses
  the standalone `muta-models-dev` crate into the `muta-providers::models_dev` module (its only consumer).
- **Builds on:** [ADR-0203](0203-remote-catalog-overlay-and-connection-gated-pipeline.md) (single-source catalog
  policy and the connection valve), [ADR-0209](0209-notification-driven-authority-architecture.md) (daemon
  in-memory authority, no client polling), [ADR-0199](0199-unified-cascading-model-resolution-architecture.md)
  (the derived route-set algebra).

## Context

A connection's model list is assembled from a compiled baseline plus one remote source. The source is chosen
per preset by `RemoteCatalogSource`
(`crates/muta-providers/src/registry/mod.rs:63`: `Endpoint(DiscoveryProtocol) | ModelsDev { provider } | None`),
with a per-connection override (`RemoteCatalogSourceOverride`,
`crates/muta-contracts/src/model.rs:372`). This single-source policy is correct (ADR-0203) and is not in question.

The problem is the layer *around* the policy. Today:

1. **Three overlapping representations of "the model list".** The compiled snapshot
   (`crates/muta-providers/src/models_dev/snapshot.json`), the on-disk raw catalog
   (`$XDG_CACHE_HOME/muta/models-dev.json`, written by the former standalone source's `fetch_and_cache`,
   `crates/muta-models-dev/src/lib.rs`), and the per-connection
   `DiscoveryCache` (`crates/muta-agent/src/catalog/discovery.rs`) (`connection_models` / `fitted_models` /
   `remote_metadata`) all describe the same thing with a non-obvious precedence:
   `provider_models` resolved *fresh disk cache → network → stale disk cache → embedded snapshot*.
   `docs/reference/paths.md` already lists `models-dev.json` as a legacy file "not read by the current code" — a
   stale row that predates ADR-0171 and directly contradicted the code. The ambiguity is real enough to have
   caused a wrong "why is model X missing" diagnosis.

2. **A transient outage can shrink a working connection.** When the network fetch fails, `provider_models`
   silently returns the embedded snapshot as `Ok`, and the reconciler replaces that connection's
   `connection_models` wholesale (`crates/muta-agent/src/catalog/discovery.rs:405`). With the disk cache present,
   the fallback is usually a "newer last-known-good" and the list does not regress; without it, the fallback is an
   older snapshot and the connection loses models it had already advertised. This is exactly the
   "transient outage must never diminish an existing catalog" hazard called out in the former source
   (`crates/muta-models-dev/src/lib.rs:145`) — and the per-connection `DiscoveryCache` is already the durable copy
   that should win.

3. **Scheduled refresh and batch apply waste work and couple connections.** `DynamicModelsDev` re-fetches the
   catalog hourly (`crates/muta-runtime/src/bootstrap.rs:244` → `spawn_refresh`,
   `crates/muta-agent/src/dynamic.rs:19`), regardless of whether any model list is actually stale. Discovery also
   collects every connection's job (`buffered`,
   `crates/muta-agent/src/catalog/discovery.rs:342`) and applies one batch at the end, so a single slow or
   timing-out connection delays the picker refresh for all of them.

The catalog source itself is already a single, per-provider declaration. Only the caching, scheduling, and
application layers are over-built.

## Decision

### 1. The source policy stays single-valued

Each connection accesses **exactly one** remote catalog source, as declared by its provider preset or its
per-connection override. There is no waterfall, no multi-source fallback, and no re-introduction of
`ProviderEndpointWithFallback`. "Access what the policy names" is the contract.

### 2. The raw source catalog is an in-memory cache, not a disk artifact

The `models.dev` source — an internal module of `muta-providers` (`models_dev`), not a standalone crate — holds
the fetched document in process memory for the daemon's lifetime. It no longer writes or reads
`$XDG_CACHE_HOME/muta/models-dev.json`; the TTL, the cross-process file lock, and the stale-disk fallback are
deleted. The embedded, committed snapshot (`crates/muta-providers/src/models_dev/snapshot.json`, refreshed by
`scripts/refresh-models-dev-snapshot.sh`) remains the offline floor for a connection that has no
`DiscoveryCache` yet.

### 3. Fetch once, fan out; never N times

The models.dev document is fetched **single-flight**: concurrent discovery jobs that select `ModelsDev` share one
in-flight request and one in-memory result, and each provider is sliced from that shared document. A first-party
`Endpoint` source fetches per its own connection (and may use its own validator).

### 4. Refresh is connection-scoped, incremental, and persisted per connection

A refresh trigger (the user's refresh command, or adding/editing a connection) runs one job per affected
connection according to its declared source. Each job reconciles the fetched catalog through the existing valve
algebra — overlap, connection/provider filter, user include/exclude (`route_models_with_providers`,
`crates/muta-agent/src/catalog/derive.rs:80`) — and persists the result for that connection as a whole-block
replacement in `DiscoveryCache`. Each completed job emits its own picker update; a slow connection never blocks
another.

### 5. Failure never diminishes a connection

When a source fetch fails, the connection **retains its existing `DiscoveryCache`**. A fallback (embedded
snapshot or otherwise) may seed a connection that has no cached list, but it must never overwrite an existing,
richer list. Failures surface as a per-connection warning.

### 6. No scheduled refresh; every sync is user- or event-initiated

The hourly `DynamicModelsDev` loop is removed. Startup renders the persisted `DiscoveryCache` plus the
seed/baseline; it does not fetch. Network catalog sync happens only on an explicit user action. The compiled
snapshot is refreshed at build time by its script and the CI freshness guard, unchanged.

## Invariants & Behavioral Boundaries

1. **One source per connection.** Every catalog fetch targets the single source its preset/override names; no
   source waterfall exists.
2. **No runtime catalog file.** `$XDG_CACHE_HOME/muta/models-dev.json` is never written or read. The only
   persisted runtime catalog is the per-connection `DiscoveryCache`; the only persisted catalog artifact is the
   committed build-time snapshot.
3. **Single-flight source fetch.** All connections selecting the same source share one in-flight fetch and one
   in-memory document.
4. **Connection-scoped application.** A refresh never applies one connection's result to another, and each
   connection's completed result is emitted on its own.
5. **No regression on failure.** A failed fetch leaves the connection's persisted model list untouched; a
   fallback may only fill an empty one.
6. **No scheduled network sync.** No timer, poll, or cadence triggers a catalog fetch; only explicit user actions
   and connection lifecycle events do.

## Alternatives considered

- **Keep the status quo (disk cache + hourly refresh + batch apply), per ADR-0171.** Rejected: it keeps three
  representations with hidden precedence, permits outage-driven shrinkage, and spends scheduled traffic.
- **Keep the disk cache, only drop the hourly refresh.** Rejected: the disk cache's unique value is
  cross-restart last-known-good, which the per-connection `DiscoveryCache` already provides at the layer that
  actually matters; the raw file and its TTL/lock stay pure overhead and ambiguity.
- **Fetch the raw catalog per connection (no sharing).** Rejected: `models.dev` publishes one monolithic
  `api.json` (verified: `/opencode-go.json` and friends redirect to the web app; only `/api.json` returns data).
  N connections on `ModelsDev` would mean N downloads of the same document.
- **Per-connection download with a shared on-disk cache to dedupe.** Rejected: reintroduces the disk artifact;
  the in-memory single-flight achieves the same dedupe for the daemon's lifetime.
- **Waterfall fallback across multiple sources (first-party → models.dev → baseline).** Rejected by ADR-0203 and
  reaffirmed here: multi-source fallback flaps capability state between synchronizations.
- **Apply all connections, then emit one picker update.** Rejected: couples unrelated connections' latency; the
  connection-scoped incremental apply is strictly better.

## Consequences

- The models.dev source is an internal `muta-providers::models_dev` module, not a standalone crate: fetch + parse
  + embedded snapshot + an in-memory, single-flight catalog. The `cache_path` / `read_cached_catalog` /
  `fetch_and_cache` persistence and the `CACHE_TTL` constant are deleted; the crate and its manifest are gone.
  A second `RemoteCatalogSource::ModelsDev` consumer (the `openai` preset) lives in the same crate, so the
  module boundary — not a crate boundary — is the correct seam.
- `DynamicModelsDev` and its `spawn_refresh` wiring are deleted; startup discovery becomes a read of the
  persisted `DiscoveryCache`, not a network pass.
- Discovery gains a streaming apply/emit path (per completed connection) and a failure policy that preserves the
  existing `DiscoveryCache`.
- `docs/reference/paths.md`'s legacy row for `models-dev.json` becomes accurate again once the file is no longer
  written; it is retained as the cleanup note.
- Trade-off: a long-running daemon no longer learns new relay models until the user refreshes. This is accepted —
  the user asked for it, and the refresh command remains the escape hatch.
- Trade-off: the first refresh in a fresh process always reaches the network (no disk last-known-good). Accepted;
  it happens only on an explicit user action, and the connection keeps its persisted list if the fetch fails.
- Migration is staged and each step is shippable alone:
  1. Add the in-memory single-flight catalog and its slice-by-provider API.
  2. Delete the on-disk persistence, TTL, and lock; keep fetch + snapshot fallback.
  3. Delete `DynamicModelsDev` + its `spawn_refresh` wiring; make startup discovery read-only.
  4. Stream per-connection discovery results and emit per completed connection.
  5. Enforce "retain existing `DiscoveryCache` on fetch failure".

## References

- ADR-0171 — three-layer model catalog and pluggable network sources (amended: disk cache and hourly refresh
  retired).
- ADR-0203 — remote catalog overlay and connection-gated pipeline (single-source policy, valve algebra).
- ADR-0209 — notification-driven daemon authority (in-memory authority, no client-side polling).
- ADR-0199 — unified cascading model resolution (derived route-set algebra).
- ADR-0182 — route-projected capabilities and distributed single source of truth.
- `docs/reference/paths.md` — legacy stray-file ledger (the `models-dev.json` row).
