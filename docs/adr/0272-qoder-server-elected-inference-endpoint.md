# 0272. Qoder Server-Elected Inference Endpoint (Region-Map Election)

- **Status:** Accepted
- **Date:** 2026-09-21
- **Implementation:** `muta-providers` (`registry::qoder::region`), `muta-llm-client` (`RequestSignerPhase::request_url`), `muta-agent` (catalog sync)
- **Builds on:** [ADR-0267](0267-transport-middleware-pipeline-and-extensible-credential-architecture.md), [ADR-0271](0271-outbound-plan-single-wire-path.md)
- **Amends:** ADR-0271 §2 (`RequestSignerPhase::request_url` signature gains the resolved auth)

---

## Context

Qoder's three international inference hosts (`api1/api2/api3.qoder.sh`) are
**not interchangeable**. Reverse-engineering the official CLI (1.1.59,
integration doc §3.1a) showed the hosts carry distinct *roles*: the center
surface (`center.qoder.sh/algo/api/v5/service/region/endpoints`, plain
Bearer, QoderEncoding-encoded response) hands the client a role map —
`inferNodes: api3`, `security: api2`. The official client adopts the
server-assigned inference node and caches it.

muta pinned `api2` — the wrong cluster. On 2026-09-21 the daily billing
counter on that cluster (`403 code 110 "Billing daily count exceeded"`)
blocked every request while `qodercli` (elected `api3`) kept working.
Controlled trials with identical credentials and identical wire bytes:
`api1`/`api3` accepted ~10/10, `api2` rejected ~10/10. The cluster
assignment is a server-side fact muta cannot negotiate around; a hardcoded
root will drift again whenever Alibaba re-points the map.

## Decision

**The server elects the transport host; muta follows, validates, and falls
back.**

1. **Election data is provider-owned identity** (ADR-0267 pattern):
   `QoderStoredIdentity`/`QoderRequestIdentity` gain
   `infer_endpoint: Option<String>`, persisted under
   `TokenSet.attributes["qoder"]`. `serde(default)` keeps pre-election
   auth stores backward-compatible.
2. **Sync once, in the existing repair critical section.** A new
   `registry::qoder::region` module implements the protocol:
   `GET center…/v5/service/region/endpoints` (plain Bearer), QoderEncoding
   decode (reuses `wire_decode`), adopt `inferNodes[0].url` **only** if it
   passes the strict https `*.qoder.sh` allowlist. It rides the same
   `ensure_qoder_uid_locked` lock region as the uid backfill (OAuth
   credentials) and the PAT source's identity mint. Any failure is
   non-fatal: the identity keeps `None` and the pin stays authoritative
   (ADR-0227: failure never diminishes a connection).
3. **The signer consumes the election.** ADR-0271 already made
   `RequestSignerPhase` own the URL rewrite; the trait signature now
   receives the resolved `auth` so a provider-owned identity can override
   the executor's base URL. Qoder's signer uses
   `identity.infer_endpoint` when present, else the pinned base. Generic
   signers ignore the argument (default unchanged).
4. **The catalog follows the same host**: a provider hook
   `catalog_root_for_connection(connection)` returns the elected endpoint
   for the catalog fetch root (`build_first_party_source`), mirroring the
   existing `build_catalog_signer` provider-named-call precedent; `None`
   keeps `spec.catalog_root()`.
5. The pinned `MODEL_PROVIDER_SPEC.root_url` **remains** (`api3`, the
   currently-issued value) as the fallback when no election was synced.

## Alternatives considered

- **Keep the hardcoded pin only.** Cheapest, and correct while the region
  map assigns `api3` — but the 110 incident is precisely the failure mode
  this reproduces on the next re-point; the runbook recovery is manual.
- **Full per-request election (sync on every resolve).** Adds a network
  round-trip to a hot path and amplifies center outages into inference
  latency for no correctness gain — the region map is effectively static
  between server deploys.
- **A generic "provider-advertised transport endpoint" framework** (new
  trait, capability enum, cache layer). One consumer (Qoder); the existing
  identity-persistence and signer-URL slots already express the semantics.
  Rejected as speculative generality; revisit only if a second provider
  grows server-issued transport roots.

## Consequences

- **Positive:** cluster rotation is followed automatically at next
  credential resolution; the 110-on-wrong-cluster failure class is
  eradicated without operator intervention; the allowlist keeps a
  compromised center response from redirecting inference off-domain.
- **Negative:** `RequestSignerPhase::request_url` signature changes
  (one in-tree implementor besides the trait default — migrated here);
  the election adds one control-plane request on first use of a
  credential.
- **Neutral:** the cached election persists across processes; operators
  can still inspect it in `auth.toml` under `attributes["qoder"]`.

## References

- Integration doc §3.1a (protocol reference + the 2026-09-21 incident)
- `muta-providers/src/registry/qoder/region.rs` (sync implementation)
- `muta-providers/src/registry/qoder/wire/signer.rs` (consumption)
- Live verification: region map decoded from the production endpoint with
  a stored `dt-` token; smoke passes against the elected `api3`.
