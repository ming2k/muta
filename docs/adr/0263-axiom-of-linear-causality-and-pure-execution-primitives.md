# 0263. The Axiom of Linear Causality: Pure Execution Primitives, Epistemic Sandboxing, and the Eradication of Background Tasks

- **Status:** Proposed
- **Date:** 2026-10-22
- **Scope:** `muta-agent`, `muta-runtime`, `muta-contracts`, `tools`
- **Deciders:** Muta Architecture Team
- **Builds on / Extends:**
  - [ADR-0255](0255-session-ir-native-causal-compaction-and-legacy-dual-track-eradication.md) (Session IR native causal compaction)
  - [ADR-0261](0261-purge-of-nanny-prompting-and-attention-frugal-context-architecture.md) (Attention-frugal context architecture)
  - [ADR-0262](0262-demand-paged-epistemic-memory-and-unified-inspect-channel.md) (Demand-paged epistemic memory and unified inspect channel)
- **Supersedes / Amends:**
  - [ADR-0190](0190-agent-as-actor-unified-task-fabric.md) (Agent as actor: unified task fabric) — completely supersedes the background task fabric, mailbox wakes, and multi-track execution regimes.
  - [ADR-0212](0212-decouple-followup-queue-and-authoritative-task-bar.md) (Decouple followup queue) — formalizes the total elimination of machine wakes and background process queues.
  - [ADR-0215](0215-tool-surface-consolidation.md) (Tool surface consolidation) — permanently deletes the `process` tool.
  - [ADR-0234](0234-authorized-background-job-continuations.md) (Authorized background job continuations) — permanently supersedes proposal; background continuations are rejected in favor of pure synchronous execution.
  - [ADR-0257](0257-unbounded-stream-guard-and-finite-execution-contract.md) (Unbounded stream guard) — amends Guard 1: eliminates background adoption; silent or unconstrained commands are terminated immediately.

---

## 1. Context and Problem Statement

For years, agent frameworks have chased an engineering mirage: **attempting to turn an AI coding agent into an operating-system init system (systemd) and a concurrent multi-agent colony**.

This mirage manifested in the Muta codebase as:
1. **The Polling Token Disaster**: Models dispatching commands with `background: true`, followed by iterative cycles of `process(action="status")` $\rightarrow$ `sleep` $\rightarrow$ `process(action="logs")`. In active development sessions (40k–100k tokens of context), each polling turn re-transmits the entire prompt prefix, consuming hundreds of thousands of redundant input tokens while producing zero productive delta.
2. **The Leaky Service Manager Fantasy**: Providing `service: true` forced the runtime to re-implement process supervision, port probing, stdout regex watchers, and ring-buffer log caches. In practice, long-running dev servers (Vite, Next.js) left zombie processes holding ports (`EADDRINUSE`), while automated tests were better served by self-terminating ephemeral test runners or developer-managed environments.
3. **StreamGuard Leaks ([ADR-0257](0257-unbounded-stream-guard-and-finite-execution-contract.md))**: When processes printed an initial banner and then went silent (e.g. listening on a port), StreamGuard's silence watchdog automatically "adopted" them into a background task track. This created un-killable orphan processes that lingered in the host operating system without explicit model awareness.
4. **Tool Surface Cognitive Distortion**: Exposing `process` with multiple exploratory actions (`status`, `wait`, `logs`, `kill`, `list`) enticed models into exploratory hyperactivity, wandering across the process table rather than adhering to deterministic causal sequences.

Following [ADR-0262](0262-demand-paged-epistemic-memory-and-unified-inspect-channel.md), off-stream cognitive history is addressable on demand via `inspect` handles (`sub:<id>`, `call:<id>`, `fold:<id>`).

This decision represents a **complete, uncompromising clean break**:
- **Background execution is eliminated entirely.**
- **Service supervision (`service: true`, `stop_service`, `process`) is deleted permanently.**
- **`run_command` is restored to the pure Unix philosophy: a single-shot, finite piped execution primitive.**
- **Subagents are formalized as strictly synchronous, read-only epistemic functions (Fork-Join).**

---

## 2. Decision Drivers

- **Zero-Token Waiting**: External execution latency (whether 1 second or 3 minutes) must never incur LLM inference costs or burn tokens on polling loops.
- **Strict Linear Causality**: The agent's cognitive trajectory must remain single-threaded, causal, and monotonic.
- **The Unix Philosophy ("Do One Thing Well")**: An AI coding assistant is a software engineering reasoning engine, not an operating system daemon supervisor.
- **Zero Orphaned OS State**: No background process, zombie PID, or hung socket may outlive a command invocation.
- **Total Cognitive Orthogonality**: Clean separation between execution (`run_command`), cognitive delegation (`spawn_agent`), and virtual memory retrieval (`inspect`).

---

## 3. Invariants & Behavioral Boundaries

- **`[INV-EXEC-01]` Pure Finite Execution Invariant**: All commands executed via `run_command` must be finite and self-terminating. The parameters `background: bool` and `service: bool` are permanently removed from the tool schema.
- **`[INV-EXEC-02]` Zero-Token Runtime Suspension**: When a finite foreground command exceeds the immediate feedback threshold, the Tokio runtime yields control and parks the turn (0 LLM tokens emitted). Upon process settlement, the runtime delivers the complete `ToolResult` in the single causal turn.
- **`[INV-EXEC-03]` Hard Guard Termination (Amending ADR-0257)**: StreamGuard's Silence Watchdog shall NEVER adopt a silent command into the background. If a process emits output and then goes silent for `idle_budget` without exiting, it is terminated immediately via `SIGTERM` $\rightarrow$ `SIGKILL`, returning a diagnostic error.
- **`[INV-EXEC-04]` Epistemic Sandboxing (Fork-Join Only)**: `spawn_agent` is strictly synchronous within the parent session. Subagents operate as pure, read-only epistemic functions in isolated context sandboxes, returning a synthesized markdown summary and an ADR-0262 `sub:<session_id>` handle. Nested subagent spawning is prohibited.
- **`[INV-EXEC-05]` Total Process Tool Eradication**: The `process` tool is permanently deleted. The agent toolset contains zero process inspection or process control tools.
- **`[INV-EXEC-06]` Pristine Cognitive Memory Boundary**: ADR-0262's `inspect` remains strictly dedicated to cognitive artifacts (`call:`, `fold:`, `sub:`). It contains zero operating system or process-level schemes.

---

## 4. Technical Architecture: The Pristine Kernel

The entire execution and cognitive surface collapses into exactly three orthogonal tools:

```
┌───────────────────────────────────────────────────────────────────────────┐
│                      The Pristine Agent Toolset                           │
├────────────────────────────┬───────────────────────┬──────────────────────┤
│ 1. 确定性执行 (Execution)  │ 2. 认知沙箱 (Sandbox) │ 3. 记忆调阅 (Recall) │
├────────────────────────────┼───────────────────────┼──────────────────────┤
│ • run_command              │ • spawn_agent         │ • inspect            │
│   - 100% 有限执行          │   - 纯同步 Fork-Join  │   - 唯一定址阅读通道 │
│   - 零Token挂起等待        │   - 纯只读探索/诊断   │   - 纯粹认知三界:    │
│   - 严格单轮进出           │   - 返回摘要+句柄     │     call: (修剪输出) │
│   - 超时当场击杀           │   - 禁止递归创建      │     fold: (折叠因果) │
│                            │                       │     sub:  (子Agent)  │
└────────────────────────────┴───────────────────────┴──────────────────────┘
```

### Primitive 1: Finite Execution (`run_command`)

#### Schema
```rust
pub struct ExecuteCommandArgs {
    #[tool(desc = "The shell command to execute. Commands MUST be finite and self-terminating.")]
    pub command: String,
    #[tool(desc = "Timeout in seconds (default 1800 = 30 minutes). Enforced by StreamGuard (ADR-0257).")]
    pub timeout: Option<u64>,
    #[tool(desc = "Set to true to bypass semantic folding and output raw unabridged command stream.")]
    pub raw: Option<bool>,
}
```

#### Execution Lifecycle
1. **Single-Turn Monotonicity**:
   The model invokes `run_command(command="cargo nextest run -p my-crate")`.
2. **Zero-Token Runtime Suspension**:
   If execution takes 45 seconds:
   - The Tokio worker awaits the child process.
   - The user's TUI displays a live stream and elapsed spinner.
   - **Zero tokens and zero network requests are sent to the LLM provider.**
   - The user retains the hard interrupt right: pressing `Ctrl+C` immediately propagates `SIGINT` to the process group.
3. **Deterministic Settlement**:
   Upon process termination, the tool returns the captured stdout/stderr. The model perceives a single, continuous, instant tool call.

#### Ephemeral Testing Pattern (Replacing Services)
If a test suite requires a running local server, it must be executed as a self-terminating composite script:
```bash
python3 -m http.server 8080 & SERVER_PID=$!
sleep 1
curl -s http://localhost:8080/health
kill $SERVER_PID
```
The subshell spawns, tests, and kills its child process internally. The outer command exits cleanly in 2 seconds, leaving zero lingering processes in the operating system.

---

### Primitive 2: Epistemic Sandboxing (`spawn_agent`)

Subagents exist exclusively for **context window protection and token compression**.

1. **Synchronous Execution**: Invoking `spawn_agent` parks the parent turn. An ephemeral child session is created in SQLite with an empty context window.
2. **Restricted Read-Only Capability**: The child agent is equipped solely with observation tools (`search_text`, `read_text`, `find_files`). It cannot invoke `run_command` to alter disk state, and it cannot invoke `spawn_agent` (preventing recursive fork bombs).
3. **Compacted Return & Causal Handle**: The child agent synthesizes its discoveries into a concise markdown briefing. The parent receives:
   ```text
   [Subagent settled (role: explore, status: ready). Full trace inspect handle: "sub:ses_019a2b3c"]

   ### Findings
   - Memory leak located in `crates/muta-runtime/src/buffer.rs:88`.
   ```
4. **Forensic Traceability**: If the parent model later requires granular stack traces discarded during compaction, it retrieves them on demand via `inspect(handle="sub:ses_019a2b3c", query="leak")`.

---

### Primitive 3: Universal Cognitive Retrieval (`inspect`)

Per [ADR-0262](0262-demand-paged-epistemic-memory-and-unified-inspect-channel.md), `inspect` is the single read-only channel for off-stream cognitive memory.

Because background services and OS jobs are permanently eradicated, `inspect` remains 100% clean and free of operating-system process leaks:
- `call:<tool_call_id>` — Pruned large tool results from CAS.
- `fold:<node_id>` — Folded causal subgraphs behind the compaction horizon.
- `sub:<session_id>` — Detailed execution traces of completed child subagents.

---

## 5. Amendments to Prior Architecture Decisions

### 1. Amendment to ADR-0257 (StreamGuard)
Section 3.1 of ADR-0257 ("Guard 1: Silence Watchdog") is amended:
* **Previous behavior**: If bytes were received before going silent, transition process to background task adoption.
* **Amended behavior**: Background adoption is deleted. If a process goes silent for `idle_budget = clamp(timeout/3, 5s, 480s)`, it is **terminated immediately** with `SIGTERM` followed by `SIGKILL`. The output collected prior to silence is returned alongside a diagnostic:
  ```text
  [Command terminated by StreamGuard: Process went silent for 15s without terminating. 
   Commands must be finite. Long-running daemons must be started by the user in an external shell.]
  ```

### 2. Permanent Deletion of ADR-0215 §Process
The `process` tool (`crates/muta-agent/src/tools/process_jobs.rs`), the background manager (`crates/muta-runtime/src/background_jobs.rs`), and the task mailbox (`crates/muta-runtime/src/task_mailbox.rs`) are decommissioned.

---

## 6. Consequences

### Positive
- **Elimination of the Polling Token Tax**: 100% of intermediate polling turns (`sleep`/`status`/`logs`) are eradicated. Long builds transition from costing 200k+ input tokens across multiple turns to exactly 1 turn.
- **Zero OS Leakage**: Zombie processes holding ports (`EADDRINUSE`) are mathematically impossible.
- **Extreme Architectural Simplicity**: Deletes thousands of lines of fragile runtime code: process managers, mailbox wakes, port sniffers, readiness matchers, and synthetic follow-up routers.
- **Unwavering Causal Monotonicity**: Conversation transcripts reflect pure cognitive progress with zero machine clutter or polling chatter.

### Negative / Trade-offs
- **Dev Servers Must Run Out-of-Band**: The model cannot leave a permanent dev server running across turns. Users must launch long-running servers in their own terminal tabs, matching standard real-world developer workflows.

---

## References

- [ADR-0190: Agent as actor — unified task fabric](0190-agent-as-actor-unified-task-fabric.md)
- [ADR-0212: Decouple user follow-up queue from background task fabric](0212-decouple-followup-queue-and-authoritative-task-bar.md)
- [ADR-0215: Tool surface consolidation](0215-tool-surface-consolidation.md)
- [ADR-0254: Epistemic observation lifecycle, ingestion folding, and near-tail invalidation](0254-epistemic-observation-lifecycle-ingestion-folding-and-near-tail-invalidation.md)
- [ADR-0255: Session IR native causal compaction and legacy dual-track eradication](0255-session-ir-native-causal-compaction-and-legacy-dual-track-eradication.md)
- [ADR-0257: Unbounded stream guard and finite execution contract](0257-unbounded-stream-guard-and-finite-execution-contract.md)
- [ADR-0261: Purge of nanny prompting and attention-frugal context architecture](0261-purge-of-nanny-prompting-and-attention-frugal-context-architecture.md)
- [ADR-0262: Demand-paged epistemic memory and unified inspect channel](0262-demand-paged-epistemic-memory-and-unified-inspect-channel.md)
