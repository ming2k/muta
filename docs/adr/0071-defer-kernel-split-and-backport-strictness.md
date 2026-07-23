# ADR-0071: Defer the kernel split; back-port strictness from the forks

- **Status:** Accepted
- **Date:** 2026-07-23

## Context

Two sibling efforts forked from this workspace at v0.20.x (`1c6a840`):

- `neenee-bak` extracted the agent kernel and provider layer into a sibling
  `wit` / `wit-providers` workspace (its own ADR-0071), flattened the
  product-family layout, and deleted `apps/quant` and `apps/editor`. Its tip
  working tree is a half-finished `wit`→`praxion` rename that does not
  compile, and the `wit` checkout it path-depends on no longer exists.
- `praxion` rebuilt the provider/runtime layer from scratch as a standalone
  library workspace. Roughly 80% of its capabilities (SSE streaming, retry
  with `Retry-After`, thinking-signature persistence, image parts,
  compaction, session persistence) already exist here in more mature,
  product-integrated form.

The split's premise — a second consumer needing a pure agent kernel — does
not exist yet. But both forks also produced real improvements independent of
the split: Copilot provider-scoped model metadata and discovery UX
(back-ported with its ADR-0070), the tui-view `Inline` payload refactor, the
`NoProvider` sentinel replacing the silent `MockProvider` fallback, dead-code
cleanup, and a set of strictness fixes (tool-argument schema validation,
stream-truncation detection, streaming Anthropic usage accounting, search
output caps, cached-token audit).

## Decision

Keep the single product-family workspace (`apps/` + `crates/{platform,
providers}`). Do not extract the agent kernel, the provider layer, or a
protocol crate into sibling repositories or new crates. Back-port the
split-independent improvements in place:

- Provider-scoped remote model metadata + OAuth modal UX (ADR-0070) and the
  tui-view `Inline` payload refactor, applied cleanly from the fork.
- `NoProvider` sentinel: the catalog returns `Option`, startup installs the
  sentinel explicitly, chat / `/btw` / queued-outbox sends fail fast with a
  user-facing error instead of reaching a non-functional mock.
- Dormant ADR-0037 §6 scaffolding (`SessionRegistry` / `SharedState`)
  removed; every method returned `Err("not yet populated")`.
- Tool call arguments are schema-validated before dispatch
  (`neenee_core::tool_validation`).
- Truncated provider streams surface as retryable errors (agent-level
  mid-tool-call detection + strict SSE tail / UTF-8 handling) instead of
  being silently accepted.
- Streaming Anthropic usage merges `message_start` with `message_delta`, so
  prompt tokens and cache discounts are booked correctly.

Revisit a kernel or protocol split only when a second product must link the
kernel without the product layer — consumer pull, not architecture push.

## Alternatives considered

- **Adopt the `wit` split wholesale (neenee-bak HEAD).** Rejected: the
  kernel has no second consumer; the split already produced a broken
  intermediate state and the loss of `apps/quant`; its own revisit condition
  is unmet.
- **Extract only a protocol crate** (the fork's `neenee-protocol`). Rejected
  for now: every in-tree consumer already depends on `neenee-core`, so the
  boundary crate would have no consumer that needs it in isolation — the
  abstraction-without-a-consumer smell. Revisit when a second frontend or
  driver needs the wire types without the full vocabulary crate.
- **Port praxion's `RetryObserver` telemetry.** Rejected: retries already
  surface through user-facing notices, `StreamDiscard` UI events, and
  tracing; a library-grade observer channel has no consumer in a product.
- **Add a Windows CI matrix** (praxion has one). Rejected for now: release
  targets and `install.sh` are Unix-only, so Windows CI would be a red job,
  not a signal. Revisit when Windows becomes a release target.

## Consequences

- The improvements land without structural churn; history stays linear and
  the quant/editor applications survive.
- Some fork ideas remain unported by choice: the single-step engine
  abstraction, strong-typed ids, and fail-closed `ProtocolState` semantics
  are documented here as future directions, not gaps.
- As the fork layouts diverge further, future cherry-picks get harder; the
  back-port window is effectively closed after this pass.
- Secret handling is hardened separately at the type level (ADR-0072).

## References

- The fork's split record: `neenee-bak` ADR-0071 (not in this repo).
- praxion ADR-0005 / 0007 / 0008 (abstraction discipline, reference
  implementations, zero-dependency implementations in the trait crate).
- [ADR-0070](0070-provider-scoped-remote-model-metadata.md) — arrived via
  this back-port.
- [ADR-0072](0072-type-level-secret-redaction.md) — companion secret-handling
  posture.
