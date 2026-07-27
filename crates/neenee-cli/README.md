# neenee

The interactive coding-agent binary: a terminal UI for running the neenee
coding agent against a local checkout.

This is the primary user-facing crate. It wires together the foundation
(`neenee-core` + `neenee-persistence`), the LLM providers (`neenee-providers`),
the orchestration loop and built-in tools (`neenee-agent`), and
the session transport (`neenee-transport`), then renders the interactive interface
via the in-house `neenee-tui-engine` rendering engine (ADR-0038).

The coding identity (`neenee_identity` / `principal_code`) lives in this
crate's `src/identity.rs` — the application layer, not the server layer
(ADR-0054). `neenee-transport` is application-neutral and holds no product name.

Run with:

```sh
cargo run -p neenee-cli
```

See the top-level [`README.md`](../../README.md) for installation and usage,
and [`docs/`](../../docs/) for the architecture and how-tos.
