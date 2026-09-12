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

## 3. Sandboxed Execution & Test Isolation

All automated test suites isolate filesystem and daemon footprints via `MUTA_HOME` (see [ADR-0121](../adr/0121-instance-isolation-for-development-and-testing.md), [ADR-0168](../adr/0168-complete-sqlite-unification-and-legacy-persistence-purge.md), and [Persistence](../explanation/persistence.md)).

> **Scope Note**: This section covers **inner-loop automated test execution**. For interactive TUI preview, cold-start user journeys, and manual acceptance scenario matrices, consult [Acceptance Criteria & User Journeys](acceptance.md).

### Automated Suite Isolation (`cargo nextest run`)

Automated tests will not contaminate host state (`~/.config/muta` or running host daemons):

- **Unit Tests**: Module-level tests operate purely on in-memory data structures and parsers.
- **TUI Snapshot Tests**: `cargo nextest run -p mutx -E 'test(snapshot_tests)'` renders against an in-memory test backend (`ratatui::backend::TestBackend`) without daemon IPC or filesystem writes.
- **Integration Tests**: Crates exercising the daemon lifecycle (e.g. `muta-runtime`) isolate their process by setting `MUTA_HOME` to a fresh temporary directory before initializing paths.
- **Workspace-wide Verification**: When running the full suite manually, you can explicitly export a clean sandbox root:
  ```bash
  export MUTA_HOME=$(mktemp -d /tmp/muta-test.XXXXXX)
  cargo nextest run --workspace
  rm -rf "$MUTA_HOME"
  ```

### Cross-Reference: Local Preview & Standalone Daemon Isolation

When running binaries directly during development rather than via the test runner:
- **`mutx` TUI Preview**: Debug builds (`cargo run -p mutx`) automatically bootstrap an isolated sandbox under `<target_dir>/muta-dev` via `ensure_dev_environment()`, ensuring zero interference with the host system.
- **`muta` Daemon**: Does **not** auto-sandbox. Direct execution without `MUTA_HOME` will collide with host paths. Always export `MUTA_HOME` and `MUTA_PORT` when running standalone daemons.
- Complete procedures, launch commands, and verification criteria are defined in [Acceptance](acceptance.md).

---

## 4. Failure Triage Matrix

| Symptom | Likely Cause | First Inspection |
|---------|--------------|------------------|
| `daemon lock already held` | Previous daemon test leaked or host daemon active | Check `MUTA_HOME` export; run `muta stop` |
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
