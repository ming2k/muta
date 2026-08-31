# Testing

How to build, run, interpret, and extend this repository's test suites.
Toolchain and crate boundaries live in
[Workspace layout](workspace-layout.md); the daemon-isolation mechanics —
why `--home` / `MUTA_HOME` is mandatory and how the sandbox is
inherited — live in [Dev and test isolation](dev-and-test-isolation.md).

## Scope

Covered: the Cargo test targets (unit and integration), the web-panel
smoke checks under `apps/web/e2e/`, the snapshot suite for the TUI, and
the isolation rules every suite relies on (`--home`, `MUTA_HOME`,
`test-path-override`).

Not covered: the pnpm-side unit tests (`vitest`) and frontend checks —
see the `web` job in `.github/workflows/ci.yml`; toolchain setup; and
manual TUI verification, which has its own playground in
[TUI component showcase](showcase.md).

## Test model

| Kind | Where | What it pins |
|------|-------|--------------|
| Unit (inline) | `#[cfg(test)] mod tests` at the foot of each source file, plus dedicated `src/tests.rs`-style modules | Per-module behavior; the bulk of the suite (2,000+ tests workspace-wide) |
| Integration | `crates/<crate>/tests/*.rs` | Cross-module and process-level behavior: runtime serve/lifecycle, agent orchestration round-trips, provider wire decoding, MCP stdio, CLI daemon spawn |
| Snapshot | `apps/tui/crates/mutx/src/snapshot_tests.rs` → `src/snapshots/*.snap` (`insta`) | Rendered TUI frames: tool step expansion, diffs, question modals |
| Doc tests | Rust fences in doc comments (sparse) | Examples in `mutx-engine` and `muta-contracts` compile |
| CLI smoke | `apps/web/e2e/cli-smoke.sh` | CLI surface contract against a live isolated daemon: retired spellings exit 2, noun-verb parsing, `--remote` validation |
| Protocol smoke | `apps/web/e2e/daemon-smoke.mjs` | Control-plane protocol over the panel transports: healthz, WS monitor, version/protocol mismatch handling |

Integration files carry a `_integration` / `_e2e` suffix in their names;
there is no separate marker or feature gate to opt in — they run as part
of the normal `cargo test` invocation.

Two tests in `crates/muta-agent/tests/webtool_e2e.rs` are
`#[ignore = "live network"]` — they hit real endpoints and run only when
explicitly selected:

```bash
cargo test -p muta-agent --test webtool_e2e -- --ignored
```

## Run tests

`cargo test` without a selector runs the default workspace member
(`mutx`). Scope to what you changed:

```bash
cargo test -p muta-agent                      # one crate
cargo test -p mutx snapshot_tests         # one module substring
cargo test -p muta-runtime --test serve_integration   # one integration file
cargo test -p muta-agent --test duplex streaming_loop  # one test by name substring
```

The full suite, as CI runs it:

```bash
export MUTA_HOME=$(mktemp -d /tmp/muta-test.XXXXXX)
cargo test --workspace --locked --no-fail-fast
./target/debug/muta daemon stop    # if any suite spawned a daemon
rm -rf "$MUTA_HOME"
```

`--no-fail-fast` mirrors CI so one failure does not hide the rest. Always
export `MUTA_HOME` for whole-suite runs: the variable carries the
sandbox into every test binary, including any that forget their own
tempdir (see [Dev and test isolation](dev-and-test-isolation.md)).

Backtraces and log output:

```bash
RUST_BACKTRACE=1 cargo test -p muta-runtime --test lifecycle_integration
cargo test -p muta-agent -- --nocapture
```

The smokes, locally (both confine themselves to a throwaway instance
root; a host daemon is never touched):

```bash
cargo build -p mutx --locked
bash apps/web/e2e/cli-smoke.sh

mkdir -p /tmp/muta-e2e-home
MUTA_HOME=/tmp/muta-e2e-home MUTA_PORT=9800 \
  ./target/debug/muta daemon start --port 9800 &
TOKEN=$(node -p "JSON.parse(require('fs').readFileSync(\
'/tmp/muta-e2e-home/muta/instance/daemon.json','utf8')).token")
DAEMON_TOKEN="$TOKEN" node apps/web/e2e/daemon-smoke.mjs
./target/debug/muta daemon stop
```

## Coverage

| Area | Representative targets | Run when | Reference |
|------|------------------------|----------|-----------|
| Session harness, serve transport | `muta-runtime` inline tests; `serve_integration`, `lifecycle_integration`, `autopilot_restore_integration` | After touching runtime, serve, daemon client | [ADR-0096](../adr/0096-unified-session-daemon.md) |
| Round/turn loop, built-in tools | `muta-agent` inline tests; `orchestration`, `duplex`, `session_round_trip` | After touching the agent loop or tools | [Rounds and turns](../explanation/agent-design/rounds-and-turns.md) |
| TUI rendering | `mutx` inline tests; `snapshot_tests` | After touching view tree, tool steps, diffs | [TUI component showcase](showcase.md) |
| Provider wire decoding | `muta-providers` `tests/wire.rs` (mockito mock server) | After protocol parsing changes | [Provider capabilities](../explanation/provider-capabilities.md) |
| Persistence and paths | `muta-persistence` inline tests; `tests/websearch_keys.rs` | After storage or `Dirs` changes | [Persistence](../explanation/persistence.md) |
| Wire contracts | `muta-contracts` inline tests; `tests/tokenizer_corpus.rs` | After wire/tokenizer changes; regenerates the panel's `wire.gen.ts` | [Crate layering](../explanation/crate-layering.md) |
| CLI surface | `apps/web/e2e/cli-smoke.sh` | After command shapes or flags change | [ADR-0121](../adr/0121-instance-isolation-for-development-and-testing.md) |
| Control-plane protocol | `apps/web/e2e/daemon-smoke.mjs` | After control-plane message or version changes | [ADR-0096](../adr/0096-unified-session-daemon.md) |

## Interpret failures

| Symptom | Inspect first |
|---------|---------------|
| Tests fail only when an installed daemon is running, or a suite hangs on connect | Isolation leak: a test reached the host daemon. Confirm the resolved instance with `daemon status --diagnostic`; re-export `MUTA_HOME` |
| `client/daemon binary mismatch` after a rebuild | Expected dev-drift gate, not a test bug — `muta daemon stop`, rerun; the daemon respawns on demand |
| Port 9800 in use, or a daemon landed on an ephemeral port | Two instances contend for the default port. Give the dev instance `MUTA_PORT` (CI uses 9800 for e2e only) |
| A TUI snapshot fails with a `.snap.new` file | Real rendering change. Review the `.snap.new`; when the new frame is correct, accept it with `INSTA_UPDATE=always cargo test -p mutx <filter>` and commit the updated `.snap` |
| Files appear under the real `~/.cache/muta` or `$XDG_CACHE_HOME` after a run | A test wrote outside its sandbox. The `test-path-override` hook exists for this; see [Add a test](#add-a-test) |
| `cargo test --workspace` fails only on the second run | Leftover state from a prior run in the instance root; wipe `MUTA_HOME` and rerun |

## Sanitizers

Not configured. CI runs no sanitizer or Miri jobs, and there is no
`cargo +nightly` sanitizer target in the repository. When a failure
smells like UB (crash inside the renderer, corruption in the grid
engine), reproduce locally with the standard Rust sanitizer invocation:

```bash
RUSTFLAGS="-Zsanitizer=address" \
  cargo +nightly test -p mutx-engine --target x86_64-unknown-linux-gnu
```

That is a local diagnostic, not a gate; do not add sanitizer invocations
to CI without a maintainer decision.

## Add a test

- Place unit tests inline at the foot of the source file
  (`#[cfg(test)] mod tests`) or in the crate's dedicated test module;
  place cross-crate or process-level tests in `crates/<crate>/tests/`
  with a descriptive file name. `_integration` marks the process-level
  ones.
- Name tests after behavior, not after the function under test.
- Sandbox every test that touches the filesystem or the daemon: either
  its own `tempfile` root, or — when the test must override the global
  `Dirs` default — the `test-path-override` feature on
  `muta-persistence`, which exposes `paths::set_test_default`. The
  override is guarded by an internal mutex
  (`TEST_OVERRIDE_GUARD`); take the guard before setting, so parallel
  tests do not interleave defaults.
- Network-facing tests use `mockito` (workspace dev-dependency) against
  a local mock server; do not hit real endpoints. Tests that must hit
  the real network are `#[ignore]`d with a reason string.
- New TUI renderings get an `insta` snapshot test in
  `snapshot_tests.rs`; commit the `.snap` file next to the suite. Accept
  an intended change with
  `INSTA_UPDATE=always cargo test -p mutx <filter>` — this
  repository does not assume `cargo-insta` is installed.
- Keep test binaries hermetic: no reliance on wall-clock time, locale,
  or the developer's config. Deterministic inputs only.

## Manual or example verification

For interactive runs, UI smoke testing, and acceptance checks during development, see [Manual verification](manual-verification.md).
The TUI component playground (`mutx showcase <component>`) renders individual modals and frames in isolation for eyeball checks;
see [TUI component showcase](showcase.md).

## See Also

- [Manual verification](manual-verification.md) — Interactive verification, acceptance runs, and CLI/TUI smoke testing
- [Dev and test isolation](dev-and-test-isolation.md) — why `MUTA_HOME`/`--home` is mandatory and how the daemon sandbox is inherited
- [Workspace layout](workspace-layout.md) — crate boundaries that test boundaries follow, and package-scoped Cargo commands
- [ADR-0121: Instance isolation for development and testing](../adr/0121-instance-isolation-for-development-and-testing.md)
- [ADR-0096: Unified session daemon](../adr/0096-unified-session-daemon.md)
