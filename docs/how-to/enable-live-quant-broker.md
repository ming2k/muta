# How to enable the live quant broker

Use the direct LongPort OpenAPI adapter when `neenee-quant` should read
Longbridge market data and send orders to a Longbridge account instead of the
built-in paper account.

`neenee-quant` uses the official Rust SDK while retaining tool admission,
local risk checks, audit records, and an explicit GUI arming step. See the
[Configuration Reference](../reference/configuration.md#quant-runtime).

## Enable LongPort OpenAPI

1. Open a Longbridge account, complete developer verification, enable OpenAPI,
   and obtain credentials from the
   [LongPort developer platform](https://open.longportapp.com/docs).

2. Export the API-key credentials for a non-interactive agent or GUI process:

   ```bash
   export LONGPORT_APP_KEY="replace-with-app-key"
   export LONGPORT_APP_SECRET="replace-with-app-secret"
   export LONGPORT_ACCESS_TOKEN="replace-with-access-token"
   ```

   Use OAuth instead by registering a client, then exporting:

   ```bash
   export NEENEE_QUANT_LONGPORT_AUTH=oauth
   export NEENEE_QUANT_LONGPORT_OAUTH_CLIENT_ID="replace-with-client-id"
   ```

   The first OAuth launch prints an authorization URL. Complete that flow in a
   browser. The official SDK persists and refreshes the token.

3. Select LongPort for both market data and brokerage. Choose the currency used
   for live account summaries and local risk checks:

   ```bash
   export NEENEE_QUANT_MARKET_DATA=longport
   export NEENEE_QUANT_BROKER=longport
   export NEENEE_QUANT_LONGPORT_ACCOUNT_CURRENCY=USD
   ```

4. Set conservative risk limits before starting the GUI or agent:

   ```bash
   export NEENEE_QUANT_RISK_MAX_ORDER_NOTIONAL=1000
   export NEENEE_QUANT_RISK_MAX_GROSS_EXPOSURE=5000
   export NEENEE_QUANT_RISK_ALLOW_SHORT_SELLING=false
   export NEENEE_QUANT_AUDIT_LOG="$HOME/.local/state/neenee/quant-audit.jsonl"
   ```

5. Start the quant GUI:

   ```bash
   cargo run -p neenee-quant-gui --features gui,longport -- --longport-live
   ```

   Use `--paper` instead to force the synthetic market-data and simulated
   broker profile, even when the shell contains live-broker environment
   variables. With no argument, the GUI reads `NEENEE_QUANT_*` and defaults to
   paper trading when those variables are unset.

6. Use LongPort symbol notation, such as `AAPL.US` or `700.HK`. Open the
   Portfolio view and refresh positions before arming trading.

## Verify the safety path

Submit an order above `NEENEE_QUANT_RISK_MAX_ORDER_NOTIONAL`. The result should
be `rejected_risk`, and LongPort should not receive the order.

For normal orders, inspect the audit log. Each submitted, rejected, or
broker-failed decision is written as one JSON object per line. LongPort remains
authoritative for final order acceptance and execution status.

## Check product and quote coverage

LongPort OpenAPI supports Hong Kong, United States, and China Connect quotes.
The current `neenee-quant` adapter targets Hong Kong and United States stocks
and ETFs with market and limit orders. It does not support cryptocurrency
symbols such as `BTCUSDT` or expose every product supported by LongPort.

Confirm the account's quote package before relying on depth or real-time
prices. OpenAPI access does not grant every market-data entitlement.

## Use a custom gateway instead

Keep `NEENEE_QUANT_BROKER=live-http` when an organization must route orders
through its own credential vault, approval service, or broker multiplexer. The
gateway contract and variables remain in the
[Configuration Reference](../reference/configuration.md#broker).
