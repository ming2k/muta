# 0202. Singleton web provider selection

- Status: Accepted
- Date: 2026-09-09
- Scope: agent/web-tools, persistence, runtime, frontend protocol
- Deciders: Muta maintainers
- Consulted: -
- Informed: -
- Related RFC: -

---

## Context and Problem Statement

ADR-0118 correctly separated research breadth (`search_web`) from page-reading
depth (`read_url`), but a later implementation added named web connection
instances and presets without wiring those records into execution. Frontends
could select `builtin`, Firecrawl, custom endpoints, or arbitrary connection
ids that the runtime did not implement. The daemon instead matched a separate
hard-coded string list and sometimes silently selected another backend.

The result violated a basic configuration invariant: a state presented as
active was not necessarily executable. It also duplicated identity, endpoint,
credential, capability, validation, and display metadata across contracts,
persistence, runtime, TUI, and web UI.

## Decision Drivers

- Every selectable state must correspond to an implemented runtime capability.
- Search and reader remain independent as required by ADR-0118.
- Web tools need one active route per axis, not multiple named instances.
- Secrets must remain outside behavior configuration and response payloads.
- Adding a provider must be compiler-visible and update one authoritative catalog.
- Migration must preserve old files and secrets without perpetuating their runtime model.

## Considered Options

- Keep connection instances and fully wire arbitrary endpoints and headers.
- Keep presets but remove custom connection instances.
- Use one finite provider selection per web axis with provider-scoped credentials.

## Decision Outcome

Chosen option: "one finite provider selection per web axis". A persisted
`WebConfig` contains one typed `WebSearchProvider` and one typed
`WebReaderProvider`. It is a cardinality-one configuration, not a process-global
singleton: each hosted runtime owns a versioned resolved snapshot so sessions
remain isolated and hot updates remain deterministic.

This decision amends ADR-0118 only where that record describes configuration
defaults and key placement. Its breadth/depth split remains binding.

### Invariants & Behavioral Boundaries

- `[INV-WEB-01]` Search and reader are independent axes; neither automatically invokes the other.
- `[INV-WEB-02]` A frontend may list only providers returned by the daemon capability catalog. No frontend-local provider registry is authoritative.
- `[INV-WEB-03]` Each axis selects exactly one compiled provider or `disabled`; named web connection instances and routing presets are not runtime concepts.
- `[INV-WEB-04]` Unknown provider values never fall back to another provider. Legacy aliases are handled only by explicit load-time migration.
- `[INV-WEB-05]` Behavior lives in `[web]` in `config.toml`; credentials live in provider-scoped maps in `credentials.toml`. Response payloads expose readiness/source status, never secret text.
- `[INV-WEB-06]` Environment credentials take precedence over stored credentials and the effective source is visible as status.
- `[INV-WEB-07]` A provider may be selected while its required token or endpoint is missing. This is a valid but unavailable setup state, never an active state.
- `[INV-WEB-08]` The runtime, validation boundary, and frontends consume the same typed provider and capability contracts.
- `[INV-WEB-09]` `web_connections.toml` is migration input only. Migration does not delete it or remove unmatched legacy credentials.
- `[INV-WEB-10]` Web updates use a revision precondition and hot-swap one fully resolved runtime snapshot.
- `[INV-WEB-11]` Legacy credential import is one-shot. Clearing a migrated credential must never cause preserved legacy data to resurrect it on a later load.
- `[INV-WEB-12]` One acknowledged update has one atomic persistence target: behavior or one credential. Mixed patches are rejected and split by clients into revisioned operations.

### Positive Consequences

- UI state and executable state cannot drift through independent provider lists.
- The Settings UX becomes provider selection plus only the fields that provider requires.
- Optional anonymous Jina use works without manufacturing a token.
- Provider additions fail visibly at compile time across exhaustive runtime matches.
- Legacy custom records are preserved for recovery while no longer influencing routing.

### Negative Consequences & Trade-offs

- Users cannot create several named instances of the same web provider.
- Arbitrary custom web relays are intentionally unsupported until they have a real typed runtime adapter and threat model.
- Selecting a required-token provider can temporarily leave the tool unavailable; the UI must label that state clearly.
- Behavior and secrets remain two files, so a UI flow that changes both takes two revisioned updates. The intermediate selection may visibly remain in `Needs setup`, but each acknowledgement covers one atomic persistence target.

## Rejected Alternatives & Negative Knowledge

### Fully wire named connection instances (Rejected)

- Why considered: it permits multiple accounts, endpoints, headers, and self-hosted variants.
- Why rejected: current web tools have cardinality-one routing and fixed backend protocols. General connection extensibility adds identity, validation, secret-resolution, SSRF, and UI complexity without a present multi-instance use case. A future need must arrive with an implemented adapter contract, not presentation-only records.

### Preset records without custom connections (Rejected)

- Why considered: it is less complex than arbitrary connections and resembles model-provider setup.
- Why rejected: a preset instance still creates two identities for one finite backend and retains unnecessary add/delete/activate lifecycle. Model connections represent multiple concurrent named pipes; web axes do not.

### Local frontend provider lists (Rejected)

- Why considered: fastest UI implementation.
- Why rejected: it caused the original `builtin active` and phantom Firecrawl states. Presentation must derive from daemon capabilities.

### Silent compatibility fallback (Rejected)

- Why considered: old or misspelled configuration continues to appear operational.
- Why rejected: it sends traffic and queries to a provider the user did not select. Migration must be explicit and fallback-free.

## Links

- [ADR-0118: Two-stage web research](0118-two-stage-web-research-search-breadth-fetch-depth.md)
- [Web tools architecture](../architecture/web-tools.md)
- [Web tools reference](../reference/tools/web.md)
