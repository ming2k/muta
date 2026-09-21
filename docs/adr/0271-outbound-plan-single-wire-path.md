# 0271. OutboundPlan: Single Wire Path with Pipeline-Mandated Request Planning

- **Status:** Accepted
- **Date:** 2026-09-21
- **Implementation:** `muta-llm-client`, `muta-providers`
- **Builds on:** [ADR-0265](0265-declarative-wire-surfaces.md), [ADR-0267](0267-transport-middleware-pipeline-and-extensible-credential-architecture.md)
- **Amends:** ADR-0267 §1 (the `RequestSignerPhase` trait signature), ADR-0265 §executor

---

## Context

The 0.50.5 → 0.50.7 pipeline refactor (ADR-0267) replaced the imperative
`build_qoder_request` executor branch with a declarative `TransportPipeline`.
The refactor migrated the *phases* (envelope, codec, signer) faithfully but
never wired the pipeline into the executor's send path: `prepare_body`,
`sign_request`, and the URL rewrite existed with **zero call sites**. Every
Qoder request was sent as a flat chat-completions JSON POST to the bare
provider root (`https://api2.qoder.sh`), which returns `404 Not Found`.

The interim repair routed shaped requests through the pipeline behind an
executor branch (`if pipeline.shapes_wire()`) and smuggled the signer's path
knowledge through an empty-string sentinel (`sign_request(seed, "", …)`).
Both are leaks and both are now eradicated by this decision.

Root causes, in order of importance:

1. **No execution guarantee.** `TransportPipeline` was decorative: attaching
   phases had no effect, and nothing — type system, tests, or review — could
   detect that the phases never ran. Phase unit tests were green; the
   integration layer was blind. No test observed outbound bytes.
2. **URL ownership was orphaned.** The inference path lived in
   `DialectSurface` (data), but no phase or executor owned the URL rewrite.
   The signer owned the signed *path* yet not the *URL* it signs.
3. **`signed_path: &str` in the trait signature** demanded that the executor
   supply knowledge it must not have.

## Decision

1. **Single wire path. `OutboundPlan`.** The executor asks the pipeline for a
   fully planned outbound request — `plan_request(body, auth) ->
   OutboundPlan { url, body_bytes, header_stamper }` — and executes exactly
   what the plan says. There is **no branch on wire shape** anywhere in the
   executor: a pass-through pipeline plans the plain chat-completions wire
   (same URL, canonical JSON, bearer headers); a shaped pipeline plans its
   dialect's wire. `build_request_for_auth`'s body/URL/header logic collapses
   into the pass-through planner; the executor retains only auth resolution,
   retry, timeout, and telemetry.
2. **The signer owns the URL.** `RequestSignerPhase` gains `fn
   request_url(&self, base_url: &str) -> String` (default: identity). The
   phase that owns the signed path owns the URL it signs. Pass-through
   planning uses the base URL unchanged.
3. **`signed_path` is deleted from the trait signature.** The signer derives
   its path from the dialect surface it is constructed with. The executor
   passes nothing.
4. **Golden-wire integration tests are mandatory.** For every shaped dialect,
   an integration test asserts the *exact* outbound request — URL, header
   set, and body bytes — against a pinned fixture. Phase-level unit tests do
   not satisfy this; the test must observe the planned request as the
   executor would send it. `[INV-WIRE-01]`.
5. **Plan-shape assertion at build time.** `TransportPipeline::build()` is
   not enough on its own; the executor asserts in debug builds that a
   pipeline attached via `with_pipeline` actually plans (URL + body differ
   from pass-through when a shaping phase is present), turning any future
   wiring omission into an immediate, local panic instead of a silent 404.

## Alternatives considered

- **Keep the `shapes_wire()` executor branch.** Rejected: reintroduces the
  exact conditional whose absence/presence caused the regression; two wire
  paths drift by construction.
- **Executor derives the URL from `OpenAiChatDialect::surface()`.** Rejected:
  re-couples the generic client to surface data and re-opens the
  provider-branch door ADR-0267 closed; also the surface is dialect-keyed
  while the pipeline is the unit actually attached per provider.
- **Do nothing beyond the interim repair.** Rejected: leaves the
  sentinel-arg leak and the untestable dual path in place; the next signed
  dialect fails identically.

## Consequences

- **Positive:** one code path for every dialect; URL and signing knowledge
  colocated in the signer; outbound bytes pinned by tests so any future
  executor/pipeline desynchronization fails CI, not production.
- **Negative:** `OutboundPlan` adds one allocation-light struct and the
  pass-through planner must reproduce the plain wire exactly (covered by the
  golden-wire test for the Standard dialect).
- **Neutral:** `for_openai_chat_dialect` becomes the canonical construction
  point for pass-through pipelines; provider registration attaches shaped
  pipelines exactly as before.

## Invariants

- **`[INV-WIRE-01]` Observed Outbound Bytes:** every dialect with a shaped
  pipeline has a golden-wire integration test asserting URL, headers, and
  body bytes end-to-end through the executor's plan path.
- **`[INV-PIPE-02]` Mandatory Planning:** the executor never constructs a
  request body or URL itself; it executes `OutboundPlan`s produced by the
  pipeline. (Amends `[INV-TRANSPORT-01]`: zero provider branches now includes
  zero wire-shape branches.)

## References

- Regression: `Qoder HTTP 404 Not Found` (flat body POSTed to
  `https://api2.qoder.sh/`), reproduced 2026-09-21 against the live endpoint.
- Interim repair (superseded by this ADR): `shapes_wire()` branch +
  empty-`signed_path` sentinel, 2026-09-21.
