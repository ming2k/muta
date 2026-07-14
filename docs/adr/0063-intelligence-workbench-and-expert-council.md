# 0063. Intelligence workbench and expert council boundary

- **Status:** Accepted
- **Date:** 2026-07-14

## Context

The quantitative GUI initially exposed market data, backtesting, portfolio,
and order actions as a thin presentation layer over `neenee-quant`. That
boundary keeps brokerage safe, but it does not provide the evidence workflow
needed before a trading decision: collecting current public information,
observing important pages over time, comparing independent analyses, and
recording a bounded conclusion.

Network intelligence and expert deliberation are not inherently trading
concerns. Putting them in `neenee-quant` would make the domain crate own web
collection and model orchestration. Putting them directly in the GUI would
make the behavior unavailable to another frontend and combine persistence,
network access, AI calls, and rendering in one crate.

The optics C library already supports the required layout and controls. Its
Rust binding did not expose headings, multiline text, the full icon set, or
theme color setters, which limited the GUI without justifying application-
specific widgets in the graphics library.

## Decision

Add `neenee-intelligence` as a reusable application-service crate and let the
quant GUI compose it with `neenee-quant`.

The public-information service has these boundaries:

- Reuse the configured `WebSearchTool` to collect results for durable topics.
- Rank and deduplicate results while retaining the last successful result for
  a topic when one refresh fails.
- Observe explicit HTTP or HTTPS links with conditional `ETag` and
  `Last-Modified` requests, falling back to a SHA-256 body fingerprint.
- Store only bounded metadata and a short text preview; reject watched bodies
  above 8 MiB.
- Persist topics, ranked results, link baselines, and change history under the
  shared XDG State policy.

The expert-review service has these boundaries:

- Run five named perspectives for each supported scenario: fundamental,
  macro, market microstructure, risk, and contrarian evidence review.
- Run an independent first round and a cross-examination second round before
  a separate, non-voting meeting manager synthesizes the conclusion.
- Build a fresh configured provider instance for every participant so
  transport conversation state cannot leak between identities.
- Require structured contributions and conclusions. Preserve invalid or
  failed responses as degraded evidence instead of treating free-form model
  output as advice.
- Persist at most 20 meeting records under XDG State.

Expert conclusions are advisory. `neenee-intelligence` has no broker or order
adapter, and the GUI never converts a meeting conclusion into an order. Order
submission remains an explicit, separately armed `neenee-quant` action.

Extend only the optics Rust binding for the missing general-purpose controls:
headings, multiline text, theme setters, and existing Lens icons. Keep product
colors, information architecture, and trading behavior in
`neenee-quant-gui`.

This decision partially supersedes ADR-0062's statement that
`neenee-quant-gui` depends only on `neenee-quant`. The broker credential and
execution boundaries in ADR-0062 remain unchanged.

## Alternatives considered

- **Add every capability to `neenee-quant`.** This keeps one application
  crate, but couples public-web and AI deliberation behavior to brokerage.
- **Implement the workflows in `neenee-quant-gui`.** This minimizes topology,
  but makes persistence and orchestration presentation-specific and difficult
  to test without a window.
- **Let one AI identity produce the final answer directly.** This is cheaper,
  but does not preserve independent positions, visible disagreement, or a
  distinct synthesis responsibility.
- **Add product-specific panels to optics.** This centralizes drawing code,
  but makes the graphics library responsible for trading and intelligence
  semantics instead of reusable primitives.

## Consequences

- The GUI becomes an application shell over two reusable services rather than
  a thin frontend for one domain crate.
- A full expert meeting makes 11 provider calls: five independent reviews,
  five cross-examinations, and one manager synthesis. Operators must account
  for model latency and cost.
- Link tracking detects body changes, not semantic importance. Dynamic pages
  can produce noisy fingerprints and still require human review.
- The intelligence archive is local state, not an audit-grade evidence store.
- Headless tests can replace search, link-observation, and AI ports with
  deterministic fakes.
- Other optics consumers gain the new general-purpose Rust APIs without
  acquiring neenee-specific components.

## References

- [How to use the intelligence workbench](../how-to/use-intelligence-workbench.md)
- [Configuration reference](../reference/configuration.md#intelligence-workbench)
- [ADR-0014](0014-xdg-persistence-architecture.md)
- [ADR-0035](0035-application-layer-split.md)
- [ADR-0062](0062-longport-openapi-quant-adapter.md)
