# Manual Verification

How to run and manually verify `muta` (CLI/Daemon) and `mutx` (TUI) during development.

This guide focuses on interactive developer acceptance and smoke-checking. For automated unit/integration test suites and CI checks, see [Testing](testing.md).

## Quick Start (Development Loop)

During daily development, always use `cargo run` for fast, incremental rebuilds and direct execution.

### 1. Run CLI commands directly

```bash
# Run muta CLI commands
cargo run -p muta -- --help
cargo run -p muta -- model list
cargo run -p muta -- prompt "Hello muta"
```

### 2. Run the TUI application

```bash
# Launch interactive TUI in your terminal
cargo run -p mutx
```

### 3. Visual TUI component checks (Showcase)

To inspect individual modals, diff views, or popups without driving a full agent session:

```bash
# Inspect a specific UI component
cargo run -p mutx -- showcase model_selector
cargo run -p mutx -- showcase diff_preview
```
See [TUI component showcase](showcase.md) for available showcase targets.

---

## Safe Sandbox Verification (Isolated Instance)

When you want to run or test features without touching your real user data (`~/.config/muta`, local SQLite databases, or running production daemons), use `--home`:

```bash
# Run with a dedicated throwaway sandbox directory
cargo run -p mutx -- --home /tmp/dev-muta
```

Or run daemon commands inside a throwaway environment:

```bash
# Start an isolated daemon
MUTA_HOME=/tmp/dev-muta cargo run -p muta -- daemon start

# Check status in the isolated environment
MUTA_HOME=/tmp/dev-muta cargo run -p muta -- daemon status

# Stop the isolated daemon
MUTA_HOME=/tmp/dev-muta cargo run -p muta -- daemon stop
```

---

## Acceptance Scenarios Checklist

| Area | What to verify | Command | Expected outcome |
|------|----------------|---------|------------------|
| **CLI Dispatch** | Basic argument routing & error handling | `cargo run -p muta -- <command>` | Clean command output or clear CLI error message |
| **Interactive TUI** | Session creation, streaming output, keyboard shortcuts | `cargo run -p mutx` | Responsive rendering, prompt input working, Ctrl+C / esc behavior correct |
| **Tool Execution** | Agent calling built-in tools (file edits, bash) | `cargo run -p mutx` -> prompt with a file task | Tool approval prompt appears, execution result displays properly in card |
| **TUI Modals & Diffs** | Layout, borders, wide CJK characters, scrolling | `cargo run -p mutx -- showcase <name>` | Pixel-clean rendering without ghost cells or misalignment |
| **Daemon Lifecycle** | Background process detached clean startup & shutdown | `MUTA_HOME=/tmp/d cargo run -p muta -- daemon start/status/stop` | Correct PID recorded, stops without orphaned processes |

---

## See Also

- [Dev and test isolation](dev-and-test-isolation.md) — Detailed mechanics of `--home` and `MUTA_HOME`
- [TUI component showcase](showcase.md) — Full catalog of TUI showcase views
- [Build and test workflow](build-and-test.md) — Build profiles and fast compilation loops
- [Testing](testing.md) — Automated tests (`cargo nextest`), CI gates, and snapshot testing
