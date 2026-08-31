# Acceptance

This document defines the real, end-to-end user journey acceptance procedures for `muta` (CLI/Daemon) and `mutx` (Interactive TUI).

It answers: **Can an authentic user build, launch, configure, and complete core agentic workflows through standard product interfaces without shortcuts or synthetic state injection?**

---

## 1. Scope & Prerequisites

- **Scope**: Production launch paths, interactive terminal sessions, headless command dispatch, tool approvals, session persistence, and daemon lifecycle management.
- **Prerequisites**:
  - Rust toolchain (pinned via `rust-toolchain.toml`, `cargo` + `rustup`).
  - Standard Unix terminal (xterm-256color or truecolor supported).
  - Configured model provider credentials (e.g. `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, `DEEPSEEK_API_KEY`, or custom provider config).
- **Host Isolation Invariant**:
  When executing acceptance verification on a development machine where a host `muta` daemon or real user data (`~/.config/muta`) may exist, **always isolate the run via `MUTA_HOME`**. Every command in this guide explicitly carries `MUTA_HOME` to guarantee 100% authentic user workflows without contending for host lockfiles/ports or polluting host state.

---

## 2. Launch & User Entry Points

### Sandbox Isolation Directory Setup
```bash
# Prepare a clean sandbox root and dedicated port
export MUTA_HOME=$(mktemp -d /tmp/muta-acceptance.XXXXXX)
export MUTA_PORT=9801
```

### Interactive TUI Entry (`mutx`)
```bash
# Standard interactive launch in sandbox
MUTA_HOME="$MUTA_HOME" cargo run -p mutx
```

### CLI Command Dispatch Entry (`muta`)
```bash
# Standard CLI dispatch in sandbox
MUTA_HOME="$MUTA_HOME" cargo run -p muta -- --help
```

---

## 3. Core User Journeys

### Journey 1: Interactive TUI Agent Session & Tool Approval

#### Steps
1. Launch `mutx` in an isolated interactive terminal:
   ```bash
   MUTA_HOME="$MUTA_HOME" cargo run -p mutx
   ```
2. Type a user prompt requesting a file read or search (e.g. `"Search for main functions in the workspace"`).
3. Observe streaming assistant response and tool intent generation.
4. When the tool approval sheet appears:
   - Use `←` / `→` or `Tab` to inspect permission parameters and hazard level.
   - Press `Enter` to approve execution.
5. Review the rendered tool result block and the assistant's synthesized summary.
6. Press `Ctrl+C` or type `/exit` to conclude the session.

#### Expected Outcome
- Pixel-clean TUI rendering without wide CJK character ghost cells or border glitches.
- Tool invocation cards expand and display structured stdout/stderr cleanly.
- Session transcript persists to disk under `$MUTA_HOME/muta/data/projects/`.

#### Pass / Fail Criteria
- **Pass**: Prompt streams smoothly, tool execution completes on approval, transcript saves without error.
- **Fail**: Process panics, terminal locks in raw mode, or tool execution silently hangs.

---

### Journey 2: Headless Command & Non-Interactive Prompts

#### Steps
1. Dispatch a non-interactive prompt via the CLI verb:
   ```bash
   MUTA_HOME="$MUTA_HOME" cargo run -p muta -- prompt "Explain the project structure in 3 sentences"
   ```
2. Inspect the terminal output.

#### Expected Outcome
- The daemon or standalone engine executes the prompt, streams the output to stdout, and exits with code 0 upon completion.

#### Pass / Fail Criteria
- **Pass**: Valid response printed to stdout, clean exit status 0.
- **Fail**: Non-zero exit code, unhandled error traceback, or leaked background processes.

---

### Journey 3: Background Daemon Lifecycle & Multi-Session Attach

#### Steps
1. Start the isolated unified daemon service:
   ```bash
   MUTA_HOME="$MUTA_HOME" MUTA_PORT=9801 cargo run -p muta -- daemon start --port 9801
   ```
2. Query the daemon health and active sessions:
   ```bash
   MUTA_HOME="$MUTA_HOME" cargo run -p muta -- daemon status --diagnostic
   ```
3. Attach the interactive client to the active daemon:
   ```bash
   MUTA_HOME="$MUTA_HOME" cargo run -p mutx -- attach
   ```
4. Disconnect from the TUI (`Esc` / `q`).
5. Terminate the background daemon:
   ```bash
   MUTA_HOME="$MUTA_HOME" cargo run -p muta -- daemon stop
   ```

#### Expected Outcome
- Daemon detaches into the background cleanly using the isolated socket and lockfile under `$MUTA_HOME/muta/instance/`.
- Status command prints PID, active port 9801, and connection health.
- Attach connects immediately without state corruption.
- Stop gracefully shuts down daemon and removes lockfiles.

#### Pass / Fail Criteria
- **Pass**: All lifecycle commands succeed cleanly with correct process state transitions.
- **Fail**: Orphaned background process, stale PID lock preventing restart, or socket connection failure.

---

### Journey 4: Model Configuration & Key Setup

#### Steps
1. Open the interactive model selector in `mutx` using `Ctrl+M` or `/models`:
   ```bash
   MUTA_HOME="$MUTA_HOME" cargo run -p mutx
   ```
2. Filter models by typing provider names (e.g. `anthropic`, `openai`, `deepseek`).
3. Press `Tab` or `e` to edit API keys / endpoint parameters in the model editor overlay.
4. Save and select the configured model as the session default.

#### Expected Outcome
- Live filtering narrows down model choices instantly.
- Key edits update local credentials under `$MUTA_HOME/muta/config/credentials.toml` securely.
- Selected model is immediately active for the subsequent turn.

---

## 4. Final Acceptance Checklist & Cleanup

- [ ] All 4 core journeys execute from cold start without manual state hacking or backdoor flags.
- [ ] Every execution explicitly carries `MUTA_HOME` to guarantee host isolation.
- [ ] No test-only hooks or synthetic mocks bypassed real network/terminal boundaries.
- [ ] Terminal state properly restores upon exit (no cursor disappearance or raw mode leakage).
- [ ] Session files and configuration adhere strictly to the XDG layout under `$MUTA_HOME`.
- [ ] Cleanup sandbox root after verification:
  ```bash
  rm -rf "$MUTA_HOME"
  ```
