# How to use the intelligence workbench

Use the quant GUI to collect public signals, observe important pages, and run
a structured multi-expert review. The intelligence features do not submit
trades.

## Configure the inputs

Configure a web-search backend in `[websearch]`. The workbench uses the same
settings as the built-in `websearch` and `webfetch` tools. See the
[Web Tool Reference](../reference/tools/web.md).

Configure `default_provider` and its credentials before using the expert
council. See the [Provider Configuration Reference](../reference/providers.md).
The council is unavailable when no configured provider can be resolved.

## Start the paper workspace

Run the GUI from the repository root:

```bash
cargo run -p neenee-quant-gui --features gui -- --paper
```

The `--paper` profile forces synthetic market data and the simulated broker.
It remains disarmed until **Arm trading** is selected.

## Collect public signals

1. Open **Intelligence**.
2. Enter a label and a focused search query, then select **Add topic**.
3. Select **Refresh network**.
4. Review ranked results under **Top signals** and any source warnings above
   the tabs.

The three default topics cover macro policy, equity catalysts, and the
technology cycle. A failed topic retains its last successful results while
other topics continue refreshing.

## Watch a link for changes

1. Open **Intelligence** and select **Watched links**.
2. Enter a label and a public `http://` or `https://` URL.
3. Select **Watch link**, then **Refresh network** to establish the baseline.
4. Refresh again later and inspect the link's state and change count.

The observer uses server validators when available and otherwise compares a
SHA-256 fingerprint. It records a changed body, not whether the change is
material. Pages with rotating timestamps, ads, or personalization can create
noise.

## Convene the expert council

1. Open **Expert council**.
2. Select an investment thesis, market event, trade risk, or strategy review
   scenario.
3. Enter one bounded decision question.
4. Paste relevant facts and source context into **Evidence and context**.
5. Select **Convene expert council**.
6. Review the manager's consensus, disagreements, actions, stop conditions,
   and each expert's second-round contribution.

A meeting makes 11 AI requests. Five experts first answer independently, then
challenge one another, and a separate meeting manager writes the final
conclusion. A provider failure or invalid structured response marks the
meeting degraded instead of silently converting the response into advice.

## Keep live execution separate

Start the real-account entry point only after configuring LongPort:

```bash
cargo run -p neenee-quant-gui --features gui -- --longport-live
```

The live profile starts disarmed. Expert conclusions never arm trading,
populate an order, or submit one. Review the [Live Quant Broker Guide](enable-live-quant-broker.md)
before enabling the real-account path.

## Inspect retained state

The workbench stores its local archive under the resolved XDG State directory:

- `intelligence/opinion.json` contains topics, ranked items, and watched-link
  baselines.
- `intelligence/expert-meetings.json` contains at most 20 recent meetings.

See the [Paths Reference](../reference/paths.md) for the platform-specific
State directory and override precedence.
