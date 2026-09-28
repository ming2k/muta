---
id: ADR-0286
title: "Universal Interactive Wait-State Containment, Physical Terminal Decoupling, and Fail-Fast Headless Execution"
status: accepted
date: 2026-10-28
scope: platform/process, platform/shell, agent/tools, contracts/execution
superseded_by: null
negative_knowledge: true
---

# 0286. Universal Interactive Wait-State Containment, Physical Terminal Decoupling, and Fail-Fast Headless Execution

- **Status:** Accepted
- **Date:** 2026-10-28
- **Scope:** `muta-platform` (`process`, `shell`, `workspace_sandbox`), `muta-agent` (`execute_command`, `episodic`), `muta-contracts` (`tool_output`, execution contracts)
- **Deciders:** Muta Architecture Team
- **Builds on:** [ADR-0257](0257-unbounded-stream-guard-and-finite-execution-contract.md) (Unbounded stream guard and finite execution contract), [ADR-0263](0263-axiom-of-linear-causality-and-pure-execution-primitives.md) (Axiom of linear causality and pure finite execution primitives)

---

## Context and Problem Statement

All shell command executions dispatched by the agent via `run_command` are strictly **unattended, autonomous, non-interactive, and finite** (ADR-0263). They execute without a human operator present to type answers or interact with TUI prompts.

However, commands executed in developer host environments frequently attempt interactive input:
- A tool spawns an editor (e.g. `git tag` under `tag.gpgsign=true` or un-messaged `git commit`).
- A command prompts for confirmation (`[y/N]`), passwords (`sudo`, `gpg`), or credentials.
- A script invokes blocking reads on `stdin` or `/dev/tty`.

Prior architectures suffered from three systemic flaws:
1. **Blind 8-Minute Silence Timeout (`idle_budget`)**:
   When an interactive prompt or editor blocked, `episodic.rs` clamped its `idle_budget` to up to **480 seconds (8 minutes)** under the blind assumption that any quiet process might be a long-running compile. An agent stuck on an editor or prompt was frozen for nearly 10 minutes before being killed.
2. **Symptom-Driven Special Casing**:
   Attempting to patch individual tools (e.g. creating custom Git-specific editor wrappers or string-matching command names) produced leaky, non-general abstractions that failed against nested scripts, compilers, or alternative version control tools.
3. **Controlling Terminal (`ctty`) Screen Pollution**:
   Child processes sharing the host session could open `/dev/tty` directly, writing raw curses escape sequences onto the operator's terminal and corrupting the agent's Ratatui/crossterm interface.

---

## Decision

We establish an uncompromised, three-pillar **Universal Interactive Wait-State Containment Architecture**:

### 1. Physical Terminal Decoupling via `setsid()`
Every owned subprocess spawned by `muta-platform::process::configure_owned` on Unix systems MUST invoke `libc::setsid()` in `pre_exec`.
- The child becomes the session leader and process-group leader of a new session (`PGID == PID`).
- The child is unconditionally detached from the host controlling terminal (`ctty`).
- Any attempt by child processes to `open("/dev/tty")` fails instantaneously with `ENXIO` (Errno 6: No such device or address).
- Output stream leakage to the host physical terminal is physically impossible; TUI rendering integrity is guaranteed.

### 2. Universal Headless Environment Baseline (Zero Tool-Specific Coupling)
Episodic subshells inject pure standard POSIX headless environment invariants without tool-specific logic:
- `CI=1`, `DEBIAN_FRONTEND=noninteractive`, `TERM=dumb`.
- `EDITOR=false`, `VISUAL=false`, `PAGER=cat`.
- `GIT_TERMINAL_PROMPT=0` (disables terminal credential prompts).
- Explicit removal of host terminal/display bindings (`GPG_TTY`, `DISPLAY`, `WAYLAND_DISPLAY`).

### 3. Active Interactive Wait-State Telemetry & Circuit Breaker
Instead of waiting blindly for 8 minutes of silence:
- When a command with closed `stdin` (`StdinPolicy::Closed`) produces zero output across a silent interval (default $\ge 5\text{s}$), the harness samples the process group's kernel activity (`sample_activity`).
- **Telemetry Invariant**:
  - If processes in the group are actively consuming CPU ticks ($\Delta\text{CPU} > 0$), the command is legitimate quiet computation (e.g. compilation, mathematical search, optimization); the execution continues up to the user budget.
  - If all processes in the group are in interruptible sleep (`state == 'S'`) with zero CPU progress ($\Delta\text{CPU} == 0$), the process group is **stalled in an interactive wait state** (waiting on an event, terminal, or input that will never arrive).
- The circuit breaker trips immediately: the process tree is terminated, and the execution returns with **`ShellTermination::InteractiveBlocked`** in seconds rather than minutes.
- The failure diagnostic directly informs the model:
  `[killed by harness: command entered an interactive wait state (blocked with zero CPU activity and no output) in a non-interactive environment. Supply flags or arguments non-interactively.]`

---

## Alternatives Considered

- **Special-casing Editor Wrappers (e.g. `headless-editor.sh`)**:
  *Rejected*: Anti-pattern. Editor invocations are merely one specific manifestation of interactive blocking. Writing custom stubs for editors ignores prompts, passwords, and custom read scripts, creating technical debt.
- **Blind Idle Budget Waiting (8-minute timeout)**:
  *Rejected*: Unacceptable developer latency. Distinguishing between quiet computation and interactive deadlock must be achieved through kernel wait-state and CPU telemetry.
- **Prompt Patching in `AGENTS.md`**:
  *Rejected*: Project documentation must not carry workstation-specific personal configuration workarounds.

---

## Consequences

### Positive
- Sub-5ms rejection for standard editor invocations via `EDITOR=false`.
- Fast detection ($\sim 5\text{–}7\text{s}$ instead of $480\text{s}$) for uncooperative processes entering interactive sleep.
- Zero TUI screen corruption via `setsid()` / `ENXIO`.
- Legitimate quiet computations (`cargo build -q`, `rustc`) continue uninterrupted due to active CPU telemetry.
- 100% tool-agnostic: applies identically to Git, Mercurial, Python, Node, sudo, or custom binaries.
