# neenee-quant

Quantitative-trading application for neenee.

An **application-layer** crate, a peer of `neenee-code`: it depends on
`neenee-agent` (so it reuses the full turn/round loop, pursuits, permission
broker) and layers on quantitative-trading domain tools — market data,
backtesting, order placement — plus a GUI.

## Layering

```text
neenee-core (domain) + neenee-store (persistence)
        ^
        |
neenee-providers (LLM) + neenee-tools (generic tools)
        ^
        |
neenee-agent (orchestration)
        ^
        |
neenee-quant ── adds quant domain tools & the GUI (`neenee-quant-gui`)
```

It is a sibling application crate alongside `neenee-code` (the coding agent),
sharing the core/store/agent foundation but adding trading-specific domain
tools and a GUI frontend.

## Longbridge integration

The opt-in `longport` feature connects directly to Longbridge through the
official LongPort OpenAPI Rust SDK. One `LongportAdapter` implements both
market-data and broker ports, so quotes and live trading share the SDK session
while the application retains local risk checks and audit records. The paper
broker remains the dependency-light default.

See [How to enable the live quant broker](../../../docs/how-to/enable-live-quant-broker.md)
for credentials, configuration, supported symbol notation, and safety checks.
