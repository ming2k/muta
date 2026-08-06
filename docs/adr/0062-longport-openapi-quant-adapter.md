# 0062. Direct LongPort OpenAPI adapter for quantitative trading

- **Status:** Superseded by ADR-0073
- **Date:** 2026-07-14

## Context

`neenee-quant` already separates market data and broker behavior behind
`MarketDataAdapter` and `BrokerAdapter`. Its production path, however, only
supports a broker-neutral HTTP gateway. Using Longbridge through that path
requires operating another signing and translation service even though
Longbridge publishes an official Rust SDK for LongPort OpenAPI.

LongPort supplies both quote and trade contexts. Creating separate provider
crates or letting `neenee-quant-gui` call the SDK would duplicate connections
and move trading behavior into the presentation layer. The OpenAPI trade
limit also requires client-side control, while local risk checks and audit
records must remain independent of the remote broker.

## Decision

Add the official `longport` Rust SDK to `neenee-quant` and implement one shared
`LongportAdapter` that satisfies both the market-data and broker contracts.

Keep the following boundaries:

- `neenee-quant` owns authentication configuration, LongPort SDK contexts,
  symbol and data conversion, trade throttling, local risk checks, and audit
  decisions.
- `neenee-quant-gui` remains a thin optics/iris presentation crate and depends
  only on `neenee-quant` domain APIs.
- API-key secrets are accepted only through environment lookup and are skipped
  by serialization and redacted from debug output. OAuth uses the SDK's token
  persistence.
- One configured adapter is shared when both market data and live brokerage
  use LongPort, avoiding duplicate quote and trade sessions.
- The built-in paper runtime remains the default. The broker-neutral
  `live-http` adapter remains available for custom gateways.

Do not add a separate LongPort crate. Provider-specific behavior is bounded
by the existing quant adapter contracts, and a new crate would add topology
without creating a reusable lower-level contract.

## Alternatives considered

- **Continue using only `live-http`.** This preserves a broker-neutral process
  boundary but requires users to deploy and maintain a gateway that duplicates
  the official SDK's signing, session, and data-model support.
- **Create `neenee-quant-longport`.** This isolates the dependency but splits a
  single application concern across another workspace member and complicates
  sharing one connection between quote and broker adapters.
- **Call LongPort from the GUI.** This couples credentials and account mutation
  to one frontend and prevents headless agents from reusing the integration.

## Consequences

- Longbridge accounts can supply quotes, candlesticks, depth, assets,
  positions, orders, and cancellation directly to `neenee-quant`.
- The quant dependency graph grows because the official SDK includes HTTP,
  WebSocket, OAuth, and protocol support.
- Live orders retain local notional, exposure, short-selling, arming, and audit
  safeguards, but the remote broker remains authoritative for final acceptance
  and execution.
- Real-account integration tests require user credentials and are not run in
  the normal test suite. The adapter boundary uses a fake client for
  deterministic conversion and risk-path tests.
- No optics source change is required for this integration; the current GUI
  primitives already express the required broker state and configuration.

## References

- [LongPort OpenAPI overview](https://open.longportapp.com/docs)
- [LongPort OpenAPI SDKs](https://open.longportapp.com/sdk)
- The quant broker and its how-to/configuration pages were removed with the
  product workspace ([ADR-0073](0073-flat-coding-focused-workspace.md)).
- [ADR-0035](0035-application-layer-split.md)
