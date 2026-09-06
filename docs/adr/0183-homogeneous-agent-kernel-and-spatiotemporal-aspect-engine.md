# 0183. Homogeneous Agent Kernel and Spatiotemporal Aspect Engine

- **Status:** Accepted
- **Date:** 2026-09-15
- **Supersedes/Unifies:** ADR-0144, ADR-0150, ADR-0167

## Context

The Muta agent architecture evolved through three major iterations:
1. **ADR-0144 (Three-Tier Hierarchy):** Defined linear agent depth (`Supervisor < Master < Runner`).
2. **ADR-0150 (Two-Axis Agent Architecture):** Split daemon governance from session production, and introduced `Steward` for internal cognitive tasks.
3. **ADR-0167 (Worker-Station Model):** Separated archetypes (`Master` vs `Runner`) from stations (`Hypervisor`, `Session`, `Subtask`), and de-stewarded internal utilities into a `CognitivePipeline`.

While ADR-0167 removed persona baggage, two fundamental architectural compromises remained unresolved:

1. **The Artificial Archetype Dichotomy (`Master` vs `Runner`):**
   In runtime truth, `Runner` is constructed as an instance of `crate::Agent`. It runs an autonomous, uncapped ReAct loop (`rounds.rs`), evaluates intents, executes tools from `ToolSet`, and adapts to environment feedback.
   Treating `Master` and `Runner` as distinct "species/archetypes" obscured the essential physical fact: **Runner is an Agent executing in a bounded child posture under recursive delegation.**

2. **Aspect Authority & Trajectory Derailment:**
   Autonomous agents suffer from **Trajectory Derailment** (getting trapped in loops, runaway destruction, or burning infinite tokens). Prior discussions oscillated between "hanging autonomous safety agents on lifecycle hooks" (which causes state-machine reentrancy and runaway latency) and "ad-hoc procedural checks." A rigorous, uncompromising definition of aspect authority and cognitive probe integration was missing.

3. **The "Harness Agent" Category Error:**
   Teams frequently conflate the execution scaffolding (**Harness**) with the cognitive actor (**Agent**), wondering if there is a "harness-level agent" responsible for the workspace. The Harness is deterministic infrastructure, not an agent.

## Decision

We make a clean, uncompromising break from legacy taxonomy and establish the **Homogeneous Agent Kernel and Spatiotemporal Aspect Engine**.

```text
┌─────────────────────────────────────────────────────────────────────────────┐
│                          Unified Entity: struct Agent                       │
│             (ReAct Loop · Tool Authority · Context · Model Driver)          │
└──────────────────────────────────────┬──────────────────────────────────────┘
                                       │ Configured at instantiation via
                                       ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│                    Execution Policy (Runtime Bounded Posture)               │
│                                                                             │
│  [ Root Posture ] (Session / Hypervisor)   [ Delegated Child Posture ]      │
│  - depth = 0                               - depth = parent_depth + 1       │
│  - allow_human_interaction = true          - allow_human_interaction = false│
│  - spawn_agent enabled                     - spawn_agent bounded by MAX     │
│  - durable session context                 - ephemeral scratchpad context   │
└──────────────────────────────────────┬──────────────────────────────────────┘
                                       │ Runs within & guarded by
                                       ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│                       Spatiotemporal Aspect Engine                          │
│                                                                             │
│  1. Pre-flight Hook       ──► PreFlightRouterTask (Latency & Tier routing)  │
│  2. Turn-Intake Hook      ──► EnvSensorTask (Git/compiler dirty hints)      │
│  3. In-flight Stream Hook ──► StreamLoopReviewerTask (Detached cutoff)      │
│  4. Tool-Gating Stack     ──► RepeatedCallGuard + DoomGuard + TokenLedger   │
│  5. Round-EOL Hook        ──► SessionTitlerTask + SessionDigestTask         │
└─────────────────────────────────────────────────────────────────────────────┘
```

### 1. Homogeneous Agent Kernel

There is strictly one cognitive entity class in the system: `struct Agent`.

- **Recursive Delegation over Heterogeneous Species:**
  Any Agent can spawn a child Agent to isolate context or divide-and-conquer a mission.
- **Mathematical Bounded Posture (`ExecutionPolicy`):**
  A child Agent differs from its parent *only* through its runtime policy:
  ```rust
  pub struct ExecutionPolicy {
      pub depth: usize,
      pub max_depth: usize,
      pub allow_human_interaction: bool,
      pub lifecycle: ContextLifecycle, // DurableSession vs EphemeralScratchpad
      pub tool_scope: ToolScopePolicy,
  }
  ```
  - **Depth Hard-Cap:** Spawning is permitted only when `depth < max_depth`. This eliminates recursive fork-bomb cascades by mathematical construction.
  - **Human Interaction Isolation:** Child agents have `allow_human_interaction = false`. They cannot directly invoke `ask_user`; any clarification must travel up the full-duplex link to the parent.
  - **Context Scratchpad Isolation:** Child agents execute in an ephemeral context window. Intermediate tool churn (reading dozens of files) stays in the child scratchpad and collapses to a structured summary upon completion, keeping the parent's transcript pristine.

### 2. Spatiotemporal Aspect Engine (Deterministic Control)

Lifecycle aspects belong strictly to the **Harness (Deterministic Scaffolding)**. They are modeled as a five-phase pipeline of deterministic hooks:

1. **Pre-flight Phase (`PreFlightHook`):**
   Evaluates user input immediately upon arrival. Determines whether the turn requires fast direct answering, standard engineering, or deep thinking/reasoning budgets.
2. **Turn-Intake Phase (`TurnIntakeHook`):**
   Inspects external environment facts (uncommitted Git diffs, active branch, build status) and injects compact, hidden system reminders before prompt assembly.
3. **In-flight Stream Phase (`InFlightStreamHook`):**
   Observes provider token stream in real time. Combines 0ms L1 sliding-window pattern detection with out-of-band L2 semantic confirmation.
4. **Tool-Gating Stack (`ToolGatingHook`):**
   Triple-barrier defense against **Trajectory Derailment**:
   - **`RepeatedCallGuard` (Temporal rut defense):** Rejects consecutive identical failed calls to break unproductive loops.
   - **`DoomGuard` (Semantic destruction defense):** Classifies mutation signatures and path hazard to prevent destructive operations.
   - **`Layered Token Ledger` (Budget runaway defense):** Enforces hard limits on spend per round and session.
5. **Round-EOL Phase (`RoundEolHook`):**
   Executes upon round convergence to persist state, synthesize session titles (first turn), and extract rolling 12-item working memory digests.

### 3. Stateless Cognitive Substrate (`CognitivePipeline`)

Aspect hooks must make semantic judgments without becoming stateful agents. The system provides a unified **`CognitivePipeline`**:

- **Pure-Function Paradigm:** Every cognitive task implements `CognitiveTask<Input, Output>` with typed inputs and structured JSON/token outputs:
  $$\text{Input Struct} \xrightarrow{\text{Single-shot LLM}} \text{Output Struct}$$
- **Zero-Tool, Zero-State:** Cognitive tasks never invoke tools and never allocate sessions or transcripts.
- **Detached Execution & Fail-Open Invariant:**
  - In-flight reviews run detached (`tokio::spawn`) without stalling the visible user rendering stream.
  - On timeout ($t \le 2000\text{ms}$), provider failure, or parse error, the hook **fails open** to a safe fallback (e.g., `StreamLoopVerdict::No`), ensuring auxiliary probes never block the primary work stream.
- **Isolated Governance Accounting:**
  Tokens consumed by cognitive tasks are logged under the system infrastructure budget, guaranteeing honest user-session accounting.

## Consequences

### Positive
- **Conceptual Purity:** Replaces artificial tier/archetype distinctions with a single, universally composable `Agent` engine.
- **Safety by Construction:** Prevents trajectory runaway, loop deadlocks, and recursive fork bombs through typed execution policies and deterministic gating.
- **Zero Latency Penalty on Core Loop:** Eliminates agent-reentrancy on hooks; cognitive probes run detached and fail-open.
- **Context Hygiene:** Subtasks operate on isolated scratchpads, preventing intermediate exploratory noise from polluting durable conversation memory.

### Negative & Mitigations
- **Sub-Agent Visibility:** Users do not directly see child scratchpads in the primary transcript view.
  - *Mitigation:* Subtask progress notes and events stream through the existing full-duplex channel to render as expandable nested activity cards in the client.

## Related Documents
- [ADR-0144: Three-tier Agent Hierarchy and Tool Pool](0144-three-tier-agent-hierarchy-and-tool-pool.md)
- [ADR-0150: Two-Axis Agent Architecture and Harness Steward](0150-two-axis-agent-architecture-and-harness-steward.md)
- [ADR-0167: Worker-Station Agent Model and Hypervisor Station Placement](0167-worker-station-agent-model-and-hypervisor.md)
