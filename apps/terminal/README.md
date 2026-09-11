# mutx

`mutx` is Muta's terminal app. It owns the interactive TUI, headless prompt
client, session attachment, dashboard, terminal clipboard integration, and
shell completions.

This subproject keeps its Rust packages under `crates/`:

- `crates/mutx` — the executable and terminal UI.
- `crates/mutx-engine` — its private retained-grid rendering engine.

The app is a client of the `muta` daemon. On local startup it checks the shared
Muta instance and starts the sibling or `PATH`-resolved `muta` binary when no
daemon is running. It never hosts daemon services inside the `mutx` process.

Run both workspace binaries, then start the app:

```bash
cargo build -p muta -p mutx
cargo run -p mutx
```

See the top-level [`README.md`](../../README.md) for installation and usage.
