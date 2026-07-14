# Build and test workflow

The repository pins its Rust toolchain in `rust-toolchain.toml`. Cargo and
rustup select it automatically from the repository root.

## Fast local loop

Root Cargo commands default to the main `neenee-code` application:

```bash
cargo check
cargo build
cargo test
```

Use package-scoped commands while changing a library or an optional
application:

```bash
cargo check -p neenee-store
cargo test -p neenee-agent
cargo check -p neenee-quant
```

Apply the same boundary to editor diagnostics. Configure rust-analyzer with
`check.command = "check"` and `check.workspace = false` so saving a file checks
its package instead of spawning a workspace-wide Clippy build. Run Clippy
explicitly before review or rely on the CI job.

The development profile keeps line tables for actionable backtraces and omits
full debug symbols from dependencies. The test profile also disables
incremental compilation because broad test runs otherwise retain a separate
incremental graph for every test target.

## Optional integrations

Headless workspace checks do not require optics or the LongPort SDK. Enable
those integrations only for work that exercises them:

```bash
cargo build -p neenee-editor --features gui
cargo run -p neenee-quant-gui --features gui -- --paper
cargo run -p neenee-quant-gui --features gui,longport -- --longport-live
```

The GUI features require a usable optics build or installation. The
`longport` feature compiles the official broker SDK and is not required for
paper trading or backtests.

## Full verification

Run workspace-wide checks before opening a pull request:

```bash
cargo fmt --all --check
cargo check --workspace --all-targets --locked
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked
```

CI may exclude work-in-progress application crates from compile and test jobs;
the workflow file is the authority for the current gating set.

## Artifact maintenance

Cargo does not garbage-collect obsolete target variants. Check the build
directory after toolchain, linker, profile, or feature changes:

```bash
du -sh target
```

Run `cargo clean` only when the cost of rebuilding is acceptable. A cleanup is
most useful immediately after stabilizing toolchain and build flags, because
artifacts produced by the previous configuration cannot be reused.
