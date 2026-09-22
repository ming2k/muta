# 0273. Upstream Availability as a Fourth Catalog Axis: Declared, Visible, Inert, Explained

- **Status:** Accepted
- **Date:** 2026-09-21
- **Scope:** `muta-contracts`, `muta-providers`, `muta-agent`, `muta-persistence`, `muta-runtime`, `mutx`, `web`
- **Deciders:** Muta maintainers
- **Builds on:** [ADR-0203](0203-remote-catalog-overlay-and-connection-gated-pipeline.md), [ADR-0230](0230-three-valued-vision-and-declared-only-gating.md), [ADR-0266](0266-declarative-remote-catalog-descriptors.md), [ADR-0270](0270-decouple-reasoning-effort-ladders-from-core-contracts.md)
- **Amends:** [ADR-0203](0203-remote-catalog-overlay-and-connection-gated-pipeline.md) §4 and §5 (availability is a distinct axis, not an admission rule)
- **Related:** [ADR-0272](0272-qoder-server-elected-inference-endpoint.md) (the `403 code 110` incident this record generalizes)

---

## Context and Problem Statement

The catalog answers three questions, and the blueprint treats them as orthogonal
axes ([Model catalog architecture](../architecture/model-catalog.md) §1):
**membership** (which ids exist), **capability** (what each can do),
**presentation** (what the picker lists). A fourth question is already being
answered by upstream providers and had no home in that model:

> **Availability — may *this account* run *this model* right now, and if not, why?**

The mechanism existed, in a form that conflated two independent declarations and
enforced nothing server-side. `DiscoveredModel.picker_enabled` /
`RemoteModelMetadata.picker_enabled` / `ProviderModelInfo.picker_enabled` formed a
channel from wire to TUI, and three shapes filled it. Three defects followed:

1. **One bit, three meanings.** An upstream `false` arrives as a *permanent*
   unsupported surface, an *entitlement* failure, or a *transient* outage.
   `picker_enabled: bool` collapsed all three, the TUI rendered every one as the
   literal string `locked`, and the activation refusal was a hardcoded
   `"This model is locked for the current plan"`
   (`apps/terminal/crates/mutx/src/event_loop/actions.rs`). The upstream reason
   was discarded and replaced with a guess — a guess that is plainly wrong for
   the ADR-0272 `403 code 110 "Billing daily count exceeded"` case, where the
   plan is fine.
2. **One bit, two vendor declarations.** Codex's `parse_codex_models` computed
   `listed = visibility == "list" && supported_in_api.unwrap_or(true)` — ANDing a
   *listing* declaration with an *access* declaration. Its own fixture made the
   error visible: `hidden-helper` is `visibility:"hide"` **and**
   `supported_in_api:true`, so a model the API supports was recorded as unusable.
   The same conflation appears in the reference documentation, which claimed
   Copilot's `model_picker_enabled:false` "excludes the model from the picker and
   channel set" when the code excludes nothing at all.
3. **No enforcement.** The only refusal in the system was one client's key
   handler. `route_models` never consulted the flag, so a declared-unavailable
   model still derived a `Channel`, `build_provider_for_model` still built a
   provider for it, and every consumer other than `mutx` would route it and take
   an unexplained upstream 4xx. A code comment asserted that "only
   pickers/registry gate on it"; nothing in `muta-providers` gated on it.

Two further gaps: a retained verdict carried no record of *when* it was observed,
so a failed refresh presented a possibly-reversed `unavailable` as current; and a
provider **refusing the connection itself** was indistinguishable from a transient
failure, because `catalog::sync::fetch_models` erased `ModelListError` with
`.map_err(|error| error.to_string())` before the fold ever saw the status code.

**`locked` must not collapse into `disabled`.** `model-catalog.md` §4 described
the flag as a membership-level capability test; it is neither. Removing an
unavailable model from the list would destroy the distinction between *nobody
offers this* and *you specifically cannot use this* — the difference between an
irrelevant fact and the most actionable fact on screen — and would make a Qoder
picker (2 of 17 entries enabled on the reference account) identical to an empty
one. It would also break `--list-models` parity rather than achieve it, and would
convert transient quota blocks into list flapping, which
`[INV-CATALOG-03]` exists to prevent.

## Decision Drivers

- **Axis integrity**: availability is a declared fact about an (account, model)
  pair — not a capability, not a membership rule, not a listing hint.
- **Declared-only, never inferred**: extends `[INV-CATALOG-01]`/`[INV-CATALOG-06]`
  and the ADR-0230 tristate stance.
- **Truthful presentation**: a refusal must state the reason it has and must not
  state a reason it does not have.
- **Client-independence**: a rule enforced only in one frontend is not enforced.
- **Sovereignty preservation**: `connection.inject` keeps meaning what ADR-0203
  `[INV-CATALOG-04]` says it means, without silently rewriting an upstream verdict.
- **Forward compatibility**: catalog payloads round-trip through
  `remote_catalog.json` and `wire.gen.ts`; every addition is additive.

## Decision Outcome

Availability becomes a fourth axis with **two independent declared predicates**,
a single daemon-side enforcement point, and no legacy alias for `picker_enabled`.

### 1. Two declared predicates replace the one conflated bit

```rust
// muta-contracts::RemoteModelMetadata (and muta-providers::DiscoveredModel)
/// Whether this account may run the model, plus the provider's own reason.
pub availability: Option<Availability>,
/// Whether the provider wants the model *listed*.
pub advertised: Option<bool>,

pub struct Availability {
    pub usable: bool,
    pub reason: Option<String>,   // verbatim; never parsed, matched, or decided on
}
```

`picker_enabled` is **deleted**, not aliased: it named a client widget and
encoded a domain verdict, and that category error is what produced both the
wrong reference doc and the wrong code comment.

`reason` is `Option<String>` and not an enum for the reason ADR-0270 gave for
`EffortLevel`: a vendor vocabulary is not a domain vocabulary. The *verdict* is
closed and tristate; the *reason* is opaque and verbatim.

### 2. Each shape declares what its own vendor actually said

| Shape | Field | Maps to |
|-------|-------|---------|
| Qoder | `enable` (bool/number) | `availability`; no reason field exists, so `reason: None` |
| Codex | `supported_in_api` | `availability` |
| Codex | `visibility == "list"` | `advertised` |
| Copilot | `policy.state` (`enabled`/`disabled`/`unconfigured`/`unknown`) | `availability` — only an explicit `disabled` is a declaration; `unconfigured`/`unknown`/absent are **undeclared** |
| Copilot | `policy.terms` | `reason`, verbatim |
| Copilot | `model_picker_enabled` | `advertised` |
| Antigravity | — | the parser already applies the provider's admission filter (dropping `deprecatedModelIds`), so it declares nothing |

The Copilot mapping follows the vendor's own published `CCAModel` shape
(`policy`, `billing`, and the `model_picker_*` family), where picker metadata and
policy state are separate concerns — the same split Codex expresses as
`visibility` vs `supported_in_api`.

### 3. The daemon enforces it once, at the route chokepoint

`muta_agent::catalog::derive::effective_availability` is the single source of
truth for "may this be run", and `build_provider_for_model` — through which every
session start, model switch, and bootstrap resolves a model — refuses to build a
provider for a verdict that says no. Any other consumer, current or future,
inherits the refusal. A client dim is decoration; it is no longer the gate.

An explicitly requested unavailable model is refused rather than silently swapped;
an unrequested connection falls through to the first model that *is* available, so
a catalogue whose head happens to be locked still yields a runnable default.

### 4. Sovereignty overrides a verdict, and must disclose

`effective_availability` applies the connection's (or provider scope's) explicit
inclusion first: an injected model stays usable. ADR-0203 `[INV-CATALOG-04]` is
untouched, and the declaration is never rewritten — `remote_metadata` keeps the
upstream verdict, while the projection reports `availability_overridden`. The row
then reads `locked upstream · overridden by you` instead of looking natively
runnable. Silent precedence is the failure mode this rule exists to prevent.

### 5. A stale verdict is marked, and a refusal is distinguished from a hiccup

- `ModelListCacheState` gains `refresh_failed`. A failed refresh still retains the
  payload (`[INV-CATALOG-03]`), but the verdict inside it is no longer represented
  as freshly confirmed: `ProviderModelInfo.availability_stale` drives an
  `as last observed · re-check failed` marker. The verdict is still enforced — a
  failure never *widens* access.
- `ModelListError::is_refusal` classifies only an explicit `401`/`403` as an
  upstream **refusal**; everything else (`404` for a misconfigured path, `429`,
  `5xx`, transport) is transient. The fold keeps the typed error instead of
  stringifying it, `ConnectionUpdate`/`CatalogSyncOutcome` carry the verdict, and
  `ConnectStatus::CatalogSyncWarning` gains `kind`
  (`CatalogSyncFailure::{Transient, Refused}`) so a connection the account may not
  use is not presented as a network hiccup.

## Invariants & Behavioral Boundaries

- **`[INV-AVAIL-01]` Four axes, four owners.** Membership is set algebra,
  capability is a tristate cascade, presentation is a projection, availability is
  a **declared verdict on an (account, model) pair**. No axis may be implemented
  by mutating another: an unavailable model remains a member, keeps its
  capabilities, and keeps its derived route shape.
- **`[INV-AVAIL-02]` Declared, never derived.** A verdict or reason is recorded
  only when a provider response carries it. Availability is never synthesized from
  model-id patterns, HTTP status codes, plan heuristics, or error text.
- **`[INV-AVAIL-03]` Reason is inert.** `reason` is never parsed, matched on,
  localized against, or read by any decision path. It is rendered or omitted.
- **`[INV-AVAIL-04]` Verdict-only gating.** Exactly the tristate `availability`
  gates enforcement; `None` is *not a declaration* and must never render as
  disabled in any client, including `wire.gen.ts` consumers.
- **`[INV-AVAIL-05]` Sovereignty survives, and must disclose.** An explicit user
  scope may override a provider's `usable:false`; the override never rewrites the
  declaration and must be surfaced.
- **`[INV-AVAIL-06]` Enforcement precedes the wire.** No client is the only
  refusal site: a route for a declared-unavailable model is not built daemon-side.
- **`[INV-AVAIL-07]` Listing is not availability.** `advertised` and
  `availability` move independently and are never ANDed.
- **`[INV-AVAIL-08]` Honest staleness.** A verdict retained across a failed
  refresh is marked stale; enforcement never depends on the mark.
- **`[INV-AVAIL-09]` Per-model only.** This axis carries verdicts about *models*.
  A whole connection being refused is expressed by the connection status channel
  (`CatalogSyncWarning { kind: Refused }`), never by marking its models.

## Consequences

- **Wire protocol 14.** `ProviderModelInfo.picker_enabled` was removed and
  replaced by `availability` (+ `availability_overridden`, `advertised`,
  `availability_stale`). Per ADR-0134 §5, removing a field an older peer would
  *silently misinterpret* — a v13 peer reads the absent `picker_enabled` as
  `None` and renders a declared-unusable model as selectable — bumps
  `PROTOCOL_VERSION`. `MIN_PROTOCOL_VERSION` stays at 12: a v13 peer is still
  served, and the daemon-side refusal (`[INV-AVAIL-06]`) is what keeps a client
  that cannot render the verdict from turning that mis-render into a wrong
  inference. Bumping the floor instead would have been the larger hammer for a
  UI-only degradation.
- **Positive:** the three meanings that shared the string `locked` are
  distinguishable when the provider states one, and honestly indistinguishable
  when it does not; a second account, plan tier, or day changes the provider's
  payload and muta's display follows with no code change; the Codex
  `hidden-helper` class is fixed; transient outages (the ADR-0272 `110` family)
  are no longer presented as permanent paywalls, and a refused connection is no
  longer presented as flaky; every non-TUI consumer inherits the refusal.
- **Negative:** a `String` reason cannot be styled deterministically (mitigated
  by one trailing ` · ` token, truncated at render, stored verbatim). Codex is
  now genuinely stricter: `supported_in_api:false` and a Copilot
  `policy.state:"disabled"` stop routing where previously only the TUI objected.
- **Known gap:** Copilot's `billing.restricted_to` is a real entitlement
  declaration that muta still does not read, because deciding it needs the
  account's own SKU, which the catalog parser never sees.
- **Not made live:** the refresh-trigger table (`model-catalog.md` §5.7) is
  unchanged, so a revoked model may stay greyed until the next trigger — now
  marked stale rather than implied current.

## Rejected Alternatives & Negative Knowledge

- **Collapsing `locked` into `disabled`** (prune the rows at fold time). Rejected:
  destroys the *nobody offers this* / *you cannot use this* distinction, loses
  `--list-models` parity, and turns transient quota blocks into list flapping.
- **A closed `Availability` enum** (`EntitlementGated`, `RateLimited`, …).
  Rejected: it is ADR-0270's mistake in reverse — a vendor taxonomy centralized in
  core contracts — and it *mandates* parsing, so a payload that only said
  "unavailable" would be reported as `RateLimited`. Closed sets are for sets muta
  owns.
- **One bit plus `availability_reason`** (keep `picker_enabled`, add a string).
  Rejected once the Codex fixture proved two independent declarations were being
  ANDed: a reason next to a conflated bit cannot express "API-supported but
  unlisted", which is a real, observed state.
- **Fixing the TUI string only.** Rejected: leaves the refusal client-side and
  requires inventing the missing information at display time.
- **Keeping `picker_enabled` as a serde alias.** Rejected: the name is the bug
  (a widget name for a domain verdict), and the project's posture is a clean break
  with no transitional shims.
- **Deriving the reason from the model id or the status code.** Rejected: this is
  `[INV-CATALOG-01]` in a new costume, and `[INV-AVAIL-02]` names it explicitly.

## References

- Blueprint: [Model catalog architecture](../architecture/model-catalog.md)
  (§1 axes, §4 availability, §5.3 parsing, §5.5 write policy, §7 invariants, §8 deltas)
- Reference: [Model metadata](../reference/model-metadata.md), [Providers](../reference/providers.md)
- Integration evidence: [Qoder provider integration](../explanation/qoder-provider-integration.md)
  §5.2a (per-account `enable` distribution, official CLI `--list-models` vs `/model` parity)
- Incident lineage: [ADR-0272](0272-qoder-server-elected-inference-endpoint.md)
- Vendor schema: `@vscode/copilot-api` `CCAModel`/`CCAModelPolicy` (the Copilot mapping in §2)
- Implementation: `muta-contracts/src/model.rs` (`Availability`),
  `muta-providers/src/list_models.rs` (shapes, `is_refusal`),
  `muta-providers/src/registry/qoder/mod.rs`, `muta-agent/src/catalog/derive.rs`
  (`effective_availability`), `muta-agent/src/catalog/mod.rs` (the gate),
  `muta-agent/src/catalog/sync.rs` (fold, fitted overlay, staleness),
  `muta-persistence/src/config.rs` (`refresh_failed`),
  `apps/terminal/crates/mutx/src/{providers.rs,event_loop/actions.rs,overlays/provider/models.rs}`,
  `apps/web/src/lib/components/ModelPicker.svelte`
