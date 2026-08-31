# Testing

This document details how to run, interpret, and extend the automated test suites for `muta` and its ecosystem crates.

---

## 1. Scope & Test Model

The automated test hierarchy verifies programmatic implementation correctness across multiple layers:

| Kind | Location | Scope & Invariants |
|------|----------|-------------------|
| **Unit Tests** | `src/**/*.rs` (`#[cfg(test)]`) | Per-module behavior, pure logic, state machines, parsers, token accounting. |
| **Integration Tests** | `crates/<crate>/tests/*.rs` | Service boundaries, daemon socket IPC, streaming loop orchestration, provider wire decoding. |
| **TUI Snapshot Tests** | `apps/tui/crates/mutx/src/snapshot_tests.rs` | Pixel/layout regression testing using `insta` snapshots for rendered TUI frames. |
| **E2E / CLI Smokes** | `apps/web/e2e/*.sh`, `*.mjs` | Subprocess contracts, flag routing, HTTP/WS control plane against an isolated daemon. |

---

## 2. Test Execution & Fast Feedback

As defined in `AGENTS.md`, always use `cargo nextest run` for fast, parallel unit and integration test execution.

### Targeted Runs (Fast Loop)

```bash
# Run unit tests for a specific crate
cargo nextest run -p muta-agent

# Run a specific integration test target
cargo nextest run -p muta-runtime --test lifecycle_integration

# Run a single test by name filter
cargo nextest run -p muta-agent -E 'test(streaming_loop)'
```

### Static Analysis & Workspace Validation

```bash
# Instant type and syntax validation (fastest)
cargo check --workspace --all-targets

# Code formatting check
cargo fmt --all --check

# Workspace linter check
cargo clippy --workspace --all-targets --locked
```

### Snapshot Testing (`insta`)

When changing TUI components or modals, snapshot tests verify frame outputs:

```bash
# Run TUI snapshot test suite
cargo nextest run -p mutx -E 'test(snapshot_tests)'

# Review and accept snapshot differences if intended
cargo insta review
```

---

## 3. Sandboxed Execution & Dev Isolation

To ensure tests and local runs never collide with a running host daemon or touch `~/.config/muta`, use `--home` / `MUTA_HOME` (see [ADR-0121](../adr/0121-instance-isolation-for-development-and-testing.md) and [Persistence](../explanation/persistence.md)):

### Isolated Test Execution
```bash
# Run full suite in isolated sandbox
export MUTA_HOME=$(mktemp -d /tmp/muta-test.XXXXXX)
cargo nextest run --workspace
rm -rf "$MUTA_HOME"
```

### Isolated Local Dev Execution
```bash
# Run isolated TUI or daemon without touching user data
cargo run -p mutx -- --home /tmp/dev-muta

# Run daemon on a distinct port
MUTA_HOME=/tmp/dev-muta MUTA_PORT=9801 cargo run -p muta -- daemon start
```

---

## 4. Failure Triage Matrix

| Symptom | Likely Cause | First Inspection |
|---------|--------------|------------------|
| `daemon lock already held` | Previous daemon test leaked or host daemon active | Check `MUTA_HOME` export; run `muta daemon stop` |
| `Snapshot mismatch` | Rendered frame layout or ANSI escape codes modified | Run `cargo insta review` to inspect the visual diff |
| `Connection refused / timeout` | Daemon IPC socket not bound in time | Inspect `RUST_BACKTRACE=1` and daemon stderr logs |
| `Wire decode parse error` | Upstream provider schema payload mismatch | Check `crates/muta-providers/tests/wire.rs` mock fixtures |
| `CJK wide character drift` | Ghost cell calculation or unicode-width mismatch | Run `mutx showcase` on affected modal |

---

## 5. Adding New Tests

1. **Unit Tests**: Place in `#[cfg(test)] mod tests` at the foot of the module file.
2. **Integration Tests**: Place in `crates/<crate>/tests/<feature>_integration.rs`. Ensure tests instantiate isolated environments using `Dirs::new_isolated()`.
3. **Determinism**: Never rely on live network endpoints unless annotated with `#[ignore = "live network"]`.
4. **Assertions**: Assert both nominal path results and explicit error variants (e.g. `Result::Err(ExpectedError)`).
