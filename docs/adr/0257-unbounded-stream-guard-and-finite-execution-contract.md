# 0257. Unbounded Stream Guard and Finite Execution Contract

- Status: Accepted
- Date: 2026-03-31
- Scope: agent/execution, tools/shell, platform
- Deciders: muta-core
- Consulted: platform, agent-runtime
- Related ADRs: [ADR-0043](0043-bash-stdin-execution-contract.md), [ADR-0190](0190-agent-as-actor-unified-task-fabric.md), [ADR-0215](0215-tool-surface-consolidation.md), [ADR-0234](0234-authorized-background-job-continuations.md), [ADR-0254](0254-epistemic-observation-lifecycle-ingestion-folding-and-near-tail-invalidation.md)

---

## Context and Problem Statement

Headless AI agents execute commands in an automated, non-interactive environment without an interactive terminal (TTY) display matrix. When an agent or user issues a continuous monitoring, polling, or streaming command (such as `intel_gpu_top -l`, `top`, `ping`, `tail -f`, or `watch`) in foreground synchronous mode:
1. The process never terminates on its own;
2. Because it emits output continuously (e.g. once per second), the platform's silence watchdog (`blocked-command guard`, which triggers only when no output is received for `timeout/3`) never fires;
3. The process runs until reaching the 30-minute default wall-clock limit (`timeout: 1800`), during which the agent turn is blocked, or until human user cancellation;
4. The append-only stream floods the collection buffer with hundreds or thousands of redundant lines, diluting context signal and wasting model token capacity.

How do we prevent unbounded streaming commands from hanging the execution loop while preserving the ability of legitimate, long-running compilation or test tasks to complete, without violating the Unix philosophy of transparent, non-intrusive execution?

## Decision Drivers

- **Zero Semantic Rewriting**: The platform runtime must never attempt black-box heuristics or regex-based magic to alter command strings (e.g. silently injecting `timeout` or `head`). Execution remains faithful, deterministic, and transparent.
- **Dual-Guard Completeness**: The execution runtime must guard against both poles of hanging processes: silence (deadlock/stdin wait) AND flood (unbounded streaming without self-termination).
- **Fast Convergence & Snapshot Preservation**: When an unbounded stream is cut off, the agent must immediately receive the captured metrics snapshot (head and tail) and actionable diagnosis, enabling single-turn recovery instead of a catastrophic 5-minute stall.
- **Axiomatic Alignment**: The model's system prompt and tool surface must explicitly codify the non-interactive execution discipline, turning implicit assumptions into explicit protocol contracts.

## Considered Options

- **Option 1: Black-box Command Rewriting**: Intercept known streaming CLI names (`top`, `intel_gpu_top`, `ping`) and inject limits (`timeout 2s`, `| head -n 20`).
- **Option 2: Aggressive Universal Foreground Timeout**: Lower the default foreground timeout from 1800s to 10s.
- **Option 3: Dual-Guard Stream Engine with Explicit Finite Execution Contract**:
  - Implement an **Unbounded Stream Guard (StreamGuard)** alongside the Silence Watchdog in foreground execution.
  - Enforce a physical line and byte budget for foreground synchronous commands: if an unmanaged command produces continuous streaming output reaching the quota threshold without exiting, terminate the process group cleanly via `SIGINT` -> `SIGKILL`.
  - Perform head-tail preservation with clear structured termination notices.
  - Codify the **Axiom of Finite Execution** in the agent's host environment instructions and tool schema.

## Decision Outcome

Chosen option: **Option 3: Dual-Guard Stream Engine with Explicit Finite Execution Contract**.

### Architecture & Mechanics

#### 1. Dual-Guard Execution Pipeline
Foreground episodic execution (`run_episodic_command`) transitions from single-axis silence detection to a dual-guard model:
- **Guard 1 (Silence Watchdog)**: Fires when no bytes are received for `idle_budget = clamp(timeout/3, 5s, 480s)`. If the command emitted output before going silent, it transitions to background adoption (ADR-0190); if silent from birth, it is terminated.
- **Guard 2 (Unbounded Stream Guard / StreamGuard)**: Fires when a foreground command that did not declare `background: true` or `service: true` matches any of three physical stream flooding signatures:
  1. **Burst Flood**: Line count reaches `SHELL_STREAM_FLOOD_LINES = 1000` or volume reaches `SHELL_STREAM_FLOOD_BYTES = 128 KB` (e.g. `yes`, runaway loops);
  2. **Metronomic Periodic Cadence**: Inter-line arrival intervals fall into stable polling cadence (`250ms..3000ms`) with structural column homogeneity for `STREAM_METRONOMIC_SAMPLE_LIMIT = 5` consecutive ticks (e.g. `intel_gpu_top -l`, `vmstat 1`, `ping 8.8.8.8`). Once 5 continuous metric samples are gathered, instantaneous snapshot sufficiency is achieved and execution converges immediately in ~5 seconds rather than minutes;
  3. **TUI Redraw**: Detection of ANSI clear/cursor-home sequences (`\x1b[H`, `\x1b[2J`, `\x1b[1;1H`) for `TUI_REDRAW_LIMIT = 2` full screen cycles (e.g. unflagged `top`, `intel_gpu_top`).
- **Action upon StreamGuard trigger**:
  1. The runtime sends a graceful termination signal (`SIGINT`) to the process group, followed by process tree termination;
  2. The collected stream is preserved using head-tail sampling (preserving table headers and the latest instantaneous snapshot);
  3. The result is marked with `ShellTermination::StreamGuard` and annotated with an actionable self-healing advisory.

#### 2. Self-Healing Advisory Protocol
When StreamGuard terminates a command, the output contains an explicit diagnostic notice:
```text
[StreamGuard: Command terminated early because it produced continuous streaming output without self-terminating (exceeded stream budget / periodic cadence limit).
Actionable Guidance: Headless shell commands must be finite. If you need an instantaneous metric snapshot, use bounded flags (e.g. `timeout 2s <cmd>`, `head`, or tool-specific one-shot flags like `top -b -n 1`). If you need continuous monitoring, dispatch with `service: true` or `background: true`.]
```

#### 3. Axiom of Finite Headless Execution in Host Environment
The system prompt policy (`system.host_environment`) and tool descriptions are updated to explicitly declare the execution axiom:
- In a headless, non-interactive environment, all foreground commands MUST be finite and self-terminating.
- Streaming and polling tools without explicit exit bounds are strictly prohibited in the foreground.

### Invariants & Behavioral Boundaries

- **`[INV-EXEC-01]` Zero Semantic Tampering**: The runtime shall never inspect, parse, or rewrite the command string to alter user or model intent. All protection is strictly physical (stream budgets, timers, signals).
- **`[INV-EXEC-02]` Process Group Propagation**: Every termination action (timeout, silence, stream guard, or user cancellation) MUST target the full process tree / process group (`-pgid`), preventing orphaned background child processes.
- **`[INV-EXEC-03]` Non-Destructive Snapshot Recovery**: StreamGuard termination MUST preserve both the initial output (head) and the most recent metric state (tail). It must never discard gathered output as an opaque error.
- **`[INV-EXEC-04]` Opt-out for Declared Long-Running Work**: The StreamGuard budget applies strictly to foreground synchronous invocations. Commands dispatched with `background: true` or `service: true` are exempt from the foreground stream cap.

### Positive Consequences

- Commands like `intel_gpu_top -l`, `ping 8.8.8.8`, or `tail -f` will no longer hold the agent loop hostage for minutes or half an hour. They are cleanly bounded within seconds.
- The model receives the instantaneous snapshot immediately (from the tail capture) along with a clear diagnostic explanation, allowing instant continuation without human cancellation.
- Long-running compilations (`cargo build`, test suites) remain unaffected as long as they operate within standard throughput or are properly partitioned.

### Negative Consequences & Trade-offs

- Commands that legitimately output more than 1,000 lines of useful log in the foreground within a single invocation must use `raw: true`, increase timeout, or pipe output to a file / `head` / `grep`.
  - *Mitigation*: The diagnostic message explicitly reminds the agent how to direct verbose outputs to files or use `read_text` / `search_text`.

## Rejected Alternatives & Negative Knowledge

### Option 1: Black-box Command Rewriting (Rejected)
- *Why considered*: Would make commands like `top` or `intel_gpu_top` "just work" without LLM adaptation.
- *Why rejected*: Violates transparency and predictability. Regex matching fails on complex shell syntax, aliases, subshells, scripts, and non-standard tool variants. Creating special cases in the engine creates technical debt and breaks when users intentionally invoke tools in unexpected ways.

### Option 2: Aggressive Universal Foreground Timeout (Rejected)
- *Why considered*: Trivial to implement (change 1800s to 15s).
- *Why rejected*: Catastrophically breaks legitimate long commands (compiling dependencies, downloading packages, full workspace test runs). Treating all commands as if they should finish in 10 seconds destroys developer workflows.

## Links

- ADR-0043: Bash stdin execution contract
- ADR-0190: Agent as actor — unified task fabric
- ADR-0215: Tool surface consolidation
- ADR-0234: Authorized background job continuations
- ADR-0254: Epistemic observation lifecycle, ingestion folding, and near-tail invalidation
