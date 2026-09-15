# 0249. Session IR Native Execution Runtime and Durable Suspension Architecture

- **Status:** Proposed
- **Date:** 2026-10-02
- **Scope:** `core/agent`, `core/runtime`, `persistence/session_ir`, `contracts/session_ir`
- **Deciders:** Muta Architecture Team
- **Extends/Implements:** ADR-0241 (Session IR), ADR-0236 (Durable Turn Commits)
- **Amends/Refines:** ADR-0009 (Autonomous loop), ADR-0030 (In-loop intervention), ADR-0247 (Trajectory loop guard)

---

## Context and Problem Statement

ADR-0241 established the canonical in-memory and persistence model for Muta: **`SessionIR` (CausalGraph + SessionState + SessionPolicy)** and a multi-pass compiler pipeline targeting LLM wire protocols. 

However, an uncompromised clean break requires addressing the execution layer itself (`muta-agent` and `muta-runtime`):
1. **The Dual-Representation Crutch (`ir_bridge.rs`)**: Currently, `muta-agent` still executes against legacy `SessionData` and flat transcripts, relying on `ir_bridge.rs` as a transitional synchronization adapter. This incurs synchronization friction and violates `[INV-SESSION-01]`.
2. **Ephemeral Suspension & Broken Pipe Risks**: Interactive checkpoints—such as user confirmation (`ask_user`), privileged tool permissions, and async subtask coordination—frequently rely on in-memory async channels (`tokio::sync::oneshot` / `mpsc`). When the client detaches, the terminal closes, or the daemon restarts, active execution contexts are either severed or left in indeterminate zombie states.
3. **Cartesian Dualism Anti-pattern ("Model = Divergence, Runtime = Convergence")**: Traditional agent frameworks attempt to hardcode rigid control graphs or enforce external hard aborts. This fights the model's intrinsic meta-cognitive and self-healing abilities, directly contradicting the lessons learned in ADR-0033 and ADR-0247.

We require a definitive, uncompromised execution architecture: **Session IR Native Runtime (MVM)**, where the agent loop operates directly on the immutable `CausalGraph`, suspensions are first-class durable database states, and convergence is achieved through embodied cognitive arbitration.

---

## Decision Drivers

- **Zero Legacy Burden**: Completely eliminate `ir_bridge.rs` and the dual-state representation (`SessionTree` vs `Transcript`).
- **Headless & Daemon-Native Durability**: Any interactive suspension (`ask_user`, permission gates) must be atomized and committed to SQLite, allowing the daemon to suspend execution tasks with zero in-memory channel leaks.
- **Embodied Convergence**: The runtime acts as the immutable physical substrate (causal facts and physical security bounds), while the LLM acts as the embodied actor navigating that substrate via cognitive arbitration.
- **Zero-Copy Multi-Pass Compilation**: Model requests must be purely compiled on-demand from the active branch of the `CausalGraph` via `compile_model_request`, honoring KV-cache prefix stability.

---

## Considered Options

- **Option 1: Evolutionary Bridge Retention**. Keep `muta-agent` executing on flat structures and continuously improve `ir_bridge.rs` sync logic.
  - *Rejected*: Violates clean-break architecture, creates state drift, and leaks presentation concerns into execution.
- **Option 2: Dynamic Workflow Graph Engine (LangGraph-style in Rust)**. Introduce a generic dynamic graph DSL with dynamic state dictionaries and node-routing interpreters.
  - *Rejected*: Severe runtime overhead, loss of Rust compile-time type safety, and forces non-linear engineering tasks into brittle, artificial DAGs (repeating the ADR-0033 anti-pattern).
- **Option 3 (Chosen): Session IR Native Step Runtime with Durable Suspension and Embodied Causal Feedback**. Rebuild the execution loop directly over `SessionIR`, transforming each inference and tool dispatch into an atomic causal step transition.

---

## Decision Outcome

Chosen option: **Option 3**.

```text
┌─────────────────────────────────────────────────────────────────────────────┐
│                          Cognitive Agent Loop                               │
│            (Next Action Decision / Autonomous ReAct Exploration)            │
└──────────────────────────────────────┬──────────────────────────────────────┘
                                       │ Pure compile request
                                       │ (compile_model_request)
                                       ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│                 SessionIR Native Step Runtime (MVM)                         │
│                                                                             │
│  [ CausalGraph ] ──► [ Pass Pipeline ] ──► [ ModelRequest (Wire Payload) ]  │
│        ▲                                                                    │
│        │ Commit StepDelta (Dialogue, ToolResult, SystemNotice, Suspension)   │
│  [ SessionState ] ◄── Active Leaf Cursor & ExecutionStatus Machine          │
└──────────────────────────────────────┬──────────────────────────────────────┘
                                       │ Atomic Delta Commit
                                       ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│                    Persistence Engine (SQLite WAL)                          │
│        Tables: `sessions`, `causal_nodes`, `session_policies`               │
└─────────────────────────────────────────────────────────────────────────────┘
```

### 1. Step-Level Causal Journaling (Zero Shadow States)

The agent loop abandons all internal flat transcript buffers. Each tick of the execution loop operates directly on `SessionIR`:

1. **Projection**: `compile_model_request(&session_ir, &config)` compiles the active causal branch into an immutable `ModelRequest`.
2. **Execution**: The model stream yields `ToolCall` or `FinalAnswer`.
3. **Journaling**:
   - Tool calls and results are recorded as `NodeKind::Dialogue(Assistant)` and `NodeKind::Dialogue(ToolResult)` nodes.
   - `session_ir.append_node(...)` updates `active_leaf` and computes the incremental `SessionDelta`.
   - `persistence.commit_session_ir_delta(&delta)` flushes mutations via $O(\Delta)$ incremental append (`[INV-SESSION-05]`).

### 2. First-Class Durable Suspension Protocol

Interactive boundaries are represented as persistent state machine transitions:

```rust
pub enum ExecutionStatus {
    Idle,
    Running { current_turn: u64 },
    Suspended(SuspensionReason),
}

pub enum SuspensionReason {
    NeedsApproval { tool_call_id: String, action: String },
    NeedsInput { prompt: String, schema: Option<serde_json::Value> },
    RetryPending { retry_token: String, attempts: u32 },
}
```

#### Protocol Flow:
1. **Encountering a Barrier**: When `ask_user` or a permission check fires:
   - A `NodeKind::Termination(Suspended { reason })` or state transition is generated.
   - `state.execution_status` transitions to `ExecutionStatus::Suspended(...)`.
   - The delta is flushed to SQLite.
   - The tokio task completes gracefully without blocking OS threads or keeping unbuffered memory channels alive.
2. **Resumption**:
   - An external wake event (from TUI, Web, or RPC) triggers `resume_session(session_id, response_payload)`.
   - The runtime reloads `SessionIR` from the database.
   - The user response is appended as a causal node, resetting `execution_status` to `Running`.
   - The step loop continues from the exact causal point of suspension.

### 3. Embodied Cognitive Convergence (Refining ADR-0247)

We explicitly reject the Cartesian dichotomy that "the model only diverges, and the runtime must enforce convergence."

- **The Substrate Contract**: The runtime provides the **inviolable physical reality** (file system snapshots, execution sandboxes, exact causal graphs, and deterministic tool schemas).
- **The Cognitive Contract**: The model drives both exploratory hypothesis generation (divergence) and self-evaluating error recovery (convergence).
- **Arbitration via Facts, Not Aborts**:
  - When `TrajectoryLoopGuard` detects repetitive non-productive cycles, it injects a `NodeKind::SystemNotice(SystemNoticePayload::Diagnostic)` node into the causal branch.
  - The model is supplied with objective forensic facts about its trajectory rather than an unceremonious hard process termination.
  - Hard aborts are reserved strictly as an infrastructure backstop (`BackoffLadder` level 3), never as an everyday workflow mechanism.

### 4. Zero-Compromise Elimination of `ir_bridge.rs`

- `crates/muta-persistence/src/session/ir_bridge.rs` is formally deprecated and scheduled for immediate deletion upon completion of `muta-agent` native integration.
- `SessionData` is reduced to an ephemeral read-only DTO for legacy protocol clients, while `SessionIR` assumes 100% exclusive authority over session lifecycle and mutation.

---

## Invariants & Behavioral Boundaries

- **`[INV-EXEC-01]` Zero Shadow State**: The agent loop must not maintain flat transcript buffers or shadow state copies. Every execution step directly appends to `SessionIR.history` (`CausalGraph`) and updates `SessionIR.state`.
- **`[INV-EXEC-02]` Pure Functional Compiler Invocation**: The runtime never assembles requests imperatively; each inference step invokes `compile_model_request(&session_ir, &config)` on demand.
- **`[INV-EXEC-03]` Durable Suspension Parity**: No interactive gate (`ask_user`, permission challenge) may hold a blocking in-memory channel across turn boundaries. Every suspension must persist to SQLite and allow complete task detachment.
- **`[INV-EXEC-04]` Forensic Resumption Fidelity**: Resuming from a suspension must be mathematically equivalent to an uninterrupted execution, with identical KV-cache prefix stability.
- **`[INV-EXEC-05]` Embodied Cognitive Feedback**: Runtime interventions for loops and budget warnings must be recorded as explicit causal graph events (`SystemNotice`), preserving causal transparency for subsequent model turns.

---

## Consequences & Positive Trade-Offs

1. **Robust Daemon Survival**: The Muta background daemon can survive process restarts, PTY disconnections, and system sleep cycles while waiting for user input, without losing conversational or tool state.
2. **Sub-Turn Time Travel**: Developers and supervisory agents can branch from any intermediate tool-step node in the causal graph by updating `active_leaf`, enabling instantaneous execution fork-and-replay.
3. **Extreme Performance & Cache Predictability**: Pure compilation directly off the causal graph ensures mathematically stable prompt prefixes, maximizing provider KV-cache hits.
4. **Architectural Purity**: Completely closes the transitional gap between ADR-0241 and active agent execution.

---

## Links

- Extends: [ADR-0241 (Session IR: Causal graph and multi-pass compiler pipeline)](0241-session-ir-causal-graph-and-compiler-pipeline.md)
- Implements: [ADR-0236 (Durable turn commits and recoverable projections)](0236-durable-turn-commits-and-recoverable-projections.md)
- Refines: [ADR-0247 (Trajectory loop guard and cognitive arbitration)](0247-trajectory-loop-guard-cognitive-arbitration-and-backoff-ladder.md)
- Supersedes: [ADR-0009 (Autonomous uncapped loop)](0009-uncapped-agentic-loop.md) (in-memory execution model portion)
