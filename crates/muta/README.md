# muta core

The `muta` binary owns the session daemon and its service-control commands.
It wires the agent runtime, persistence, providers, local control plane, and
health endpoint without depending on either interactive frontend or bundling
their assets.

Terminal interaction lives in [`apps/tui`](../../apps/tui), whose `mutx`
binary connects to this daemon. The browser app lives in
[`apps/web`](../../apps/web) and uses the same control protocol.

Run with:

```sh
cargo run -p muta -- daemon start --fg
```

See the top-level [`README.md`](../../README.md) for installation and usage,
and [`docs/`](../../docs/) for the architecture and how-tos.
