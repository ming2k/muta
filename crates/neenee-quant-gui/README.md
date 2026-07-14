# neenee-quant-gui

GUI decision workspace for quantitative trading and supporting intelligence.

The application shell composes `neenee-quant` for market data, backtesting,
portfolio, risk, and execution with `neenee-intelligence` for public-web topics,
watched-link changes, and structured expert meetings. Brokerage credentials,
risk checks, and order mutation stay inside `neenee-quant`; expert conclusions
never submit or prefill orders.

The optional `gui` feature renders through the workspace's optics/iris stack.
The frontend does not own broker credentials or call LongPort directly; it
shows the configured runtime and forwards all market, portfolio, and order
actions to `neenee-quant`.

Run the simulated paper profile explicitly with:

```bash
cargo run -p neenee-quant-gui --features gui -- --paper
```

Run the LongPort live profile after configuring credentials with:

```bash
cargo run -p neenee-quant-gui --features gui,longport -- --longport-live
```

Live trading always starts disarmed. With no profile argument, the GUI reads
`NEENEE_QUANT_*`; an unset environment resolves to the paper profile. The GUI
binary embeds the runtime search path for a system optics installation or the
local sibling `../optics/build` tree, so `LD_LIBRARY_PATH` is not required.

See [`neenee-quant`](../neenee-quant) for the trading domain and
[`neenee-intelligence`](../neenee-intelligence) for the decision-support
services.
