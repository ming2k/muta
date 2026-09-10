# 0228. Direct network access and consistent catalog refresh

- **Status:** Accepted
- **Date:** 2026-09-10
- **Amends:** [ADR-0200](0200-owned-transport-and-packet-level-request-trace.md) — removes application proxy selection.
- **Builds on:** [ADR-0227](0227-connection-scoped-catalog-refresh-and-in-memory-source-cache.md).

## Context

Inference exposed an environment-selected proxy while provider service requests
connected directly. Web tools exposed a separate proxy configuration. models.dev
constructed another HTTP client and used a different timeout from endpoint
discovery. This made network behavior depend on which feature issued a request.
Catalog timeout errors omitted the failed phase. The source refresh mutex
serialized overlapping refreshes instead of sharing their results across passes.

## Decision

Muta supports direct application network access. Remove `MUTA_PROXY`, the proxy
connector constructor, and the web proxy configuration, update/view fields, and
UI display. The optional reqwest comparison client also disables automatic proxy
selection. Legacy web config keys deserialize as unknown fields and disappear
when saved; no proxy compatibility adapter is retained.

Inference and provider services use common direct client construction with
platform certificate verification. models.dev uses the same bounded HTTP helper
as endpoint discovery, OAuth, and usage requests. Catalog requests have a single
10-second deadline covering request and body; timeout diagnostics name the last
observed transport phase and omit URL queries. Streaming model calls retain
their distinct streaming deadline policy. Web readers retain public-address
confinement and explicit redirect validation.

The models.dev source stores one shared in-flight future. Overlapping refreshes
receive the same success or failure; a later refresh replaces a completed future.
Cancellation of one waiter does not cancel another waiter's work. If all waiters
leave, a later caller can resume the pending future. Successful fetches replace
the memory catalog; failures preserve it and the per-connection discovery state.
Catalog reads use memory or the compiled snapshot and never fetch implicitly.

## Invariants & Behavioral Boundaries

1. No application proxy setting selects an egress route.
2. Provider endpoint and models.dev discovery use the same direct HTTP helper
   and overall deadline; model streaming is not given a catalog deadline.
3. Concurrent source refresh callers share failures as well as successes.
4. Reading a catalog never initiates network activity.
5. Failed refreshes preserve existing connection model lists.

## Rejected Alternatives & Negative Knowledge

- **Add proxy support to every caller:** rejected because the project does not
  support application proxies; it expands configuration and transport branches.
- **Only increase timeouts:** rejected because it does not resolve inconsistent
  paths, duplicate requests, or the inability to identify a stalled phase.
- **Keep a mutex around each fetch:** rejected because it queues duplicate
  requests and repeats the same failure for overlapping callers.
- **Restore timed or disk-backed source refresh:** rejected because connection
  discovery already owns durable state and refresh is explicitly initiated.

## Consequences

The public Rust proxy constructor and optional web wire fields are removed.
Wire protocol and minimum supported protocol advance to 12, so old peers cannot
submit proxy updates that would be silently ignored. Upgrade clients and daemon
together.
Existing web config files continue to load, but their old proxy key has no effect.
The compiled snapshot remains an offline floor; it cannot replace a working
connection list on a failed refresh. This change improves diagnostics and request
consistency; it does not establish the historical cause of the reported outage.
