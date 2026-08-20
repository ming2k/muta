# Build and test workflow

The repository pins its Rust toolchain in `rust-toolchain.toml`. Cargo and
rustup select it automatically from the repository root.

## Fast local loop

Root Cargo commands default to the main `neenee` application:

```bash
cargo check
cargo build
cargo test
```

Use package-scoped commands while changing a library:

```bash
cargo check -p neenee-persistence
cargo test -p neenee-agent
```

Apply the same boundary to package diagnostics. Configure rust-analyzer with
`check.command = "check"` and `check.workspace = false` so saving a file checks
its package instead of spawning a workspace-wide Clippy build. Run Clippy
explicitly before review or rely on the CI job.

The development profile keeps line tables for actionable backtraces and omits
full debug symbols from dependencies. The test profile also disables
incremental compilation because broad test runs otherwise retain a separate
incremental graph for every test target.

## Full verification

Run workspace-wide checks before opening a pull request:

```bash
cargo fmt --all --check
cargo check --workspace --all-targets --locked
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked
```

CI gates every workspace member; the workflow file is the authority for the
current gating set.

## Isolation from the host installation

If an installed neenee daemon is running on this machine, run the checks
above with `NEENEE_HOME` exported to a throwaway root, and any manual run of
the debug binary with `--home <dir>`. Either confines the entire instance —
sessions, config, daemon socket/lock, logs, port — so a checkout can never
read, write, stop, or spawn into the installed daemon's state.
See [Dev and test isolation](dev-and-test-isolation.md) for the recipes.

## Artifact maintenance

Cargo does not garbage-collect obsolete target variants. Check the build
directory after toolchain, linker, profile, or feature changes:

```bash
du -sh target
```

Run `cargo clean` only when the cost of rebuilding is acceptable. A cleanup is
most useful immediately after stabilizing toolchain and build flags, because
artifacts produced by the previous configuration cannot be reused.
