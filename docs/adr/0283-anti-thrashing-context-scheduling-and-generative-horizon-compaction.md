---
id: ADR-0283
title: "Anti-Thrashing Context Scheduling and Generative Horizon Compaction"
status: proposed
date: 2026-10-25
scope: agent/orchestration, contracts/pressure, contracts/session_ir, runtime/compactor
superseded_by: null
negative_knowledge: true
---

# 0283. Anti-Thrashing Context Scheduling and Generative Horizon Compaction

- **Status:** Proposed
- **Date:** 2026-10-25
- **Scope:** `muta-agent` (orchestrator, context scheduler), `muta-contracts` (pressure budgets, Session IR), `muta-runtime` (causal compactor)
- **Deciders:** Muta Architecture Team
- **Builds on:** [ADR-0241](0241-session-ir-causal-graph-and-compiler-pipeline.md) (Session IR causal graph), [ADR-0277](0277-unified-context-planner-and-request-compiler.md) (Unified context planner and request compiler), [ADR-0278](0278-compaction-as-a-view-commit.md) (Compaction as a view commit), [ADR-0280](0280-versioned-context-policy-and-clean-break-cutover.md) (Versioned context policy and clean-break cutover), [ADR-0282](0282-tool-observation-claim-check-lifecycle-and-cas-backing-store.md) (Tool observation claim-check lifecycle and CAS backing store)
- **Amends:** [ADR-0255](0255-session-ir-native-causal-compaction-and-legacy-dual-track-eradication.md) (Session IR native causal compaction)

---

## Context and Problem Statement

As multi-turn autonomous agent sessions progress across dozens of turns, naive context reduction strategies trigger severe system pathologies:

1. **The Critical-Threshold Thrashing Cascade (Cache Thrashing)**:
   In naive implementations governed by a single watermark (e.g. `if total_tokens > 65% { prune(); }`), context hovers directly around the boundary. Every turn crosses the threshold, triggers a micro-prune of trivial scraps (e.g. 100–300 tokens), and drops context back to 64.8%. The subsequent turn crosses 65% again. Because pruning mutates intermediate historical messages, **every single turn destroys the KV-cache prefix**, causing catastrophic TTFT latency spikes, 10x API cost inflation, and loss of remote continuation cursors (`previous_response_id`).
2. **The "Pruning Will Suffice" Delusion (The Pruning Floor)**:
   Pruning is fundamentally constrained to tool results. It cannot touch user prompts, assistant reasoning, architectural plans, or in-flight tool call arguments. As turns accumulate, User and Assistant text alone easily reaches 80,000–120,000 tokens. Once historical tool results have already been cleared to 20-token tombstones, the marginal yield of pruning drops to **zero**. A system without an orthogonal macro-compaction tier suffers fatal context exhaustion.
3. **The Category Mistake of Treating Compaction as a Claim-Check**:
   Compaction cannot be modeled as an "addressable tool invoice". Compacting 40 turns of human dialogue, tentative design drafts, and tool interactions is an **irreversible, generative semantic state transition**. Attempting to "page in" folded turns into active context would immediately trigger the exact OOM context limit compaction was invoked to escape.

An uncompromised context architecture must separate **deterministic tool-level garbage collection (Pruning)** from **generative dialogue-level epoch advancement (Compaction)**, and mathematically guarantee **Zero Thrashing**.

---

## Decision

We establish the **Anti-Thrashing Context Scheduling Engine** and formalize **Generative Horizon Compaction as an Epochal Savepoint Transition**.

```text
 100% ────────────────────────────────────────────────────────── Physical Model Limit
  85% ──── [ Hard Watermark W_compact ] ───────────────────────► Generative Epoch Compaction
   │                                                             (LLM summarizes [0..H] into BeliefState;
   │      [ Protected Cruise Zone ]                              resets active context to W_target = 25%)
   │      - KV-Cache 100% Hot Hit Rate
   │      - Zero Pruning / Zero Jitter
   │
  70% ──── [ High Watermark W_high ] ──► Prune Gate Check:
   │                                     Only triggers if Reclaimable >= Q_min (4,000 tokens)
   │                                     AND can land within W_low (50%)
   │                                     Otherwise: Trip Exhaustion Circuit-Breaker!
  50% ──── [ Low Watermark W_low ] ────► Pruning Target Landing Zone
   │
  25% ──── [ Target Watermark W_target ] ► Compaction Landing Zone
```

---

### 1. The Anti-Thrashing Engine: Four Mathematical Dampers

To eliminate critical-threshold oscillation and protect KV-cache economics, context scheduling obeys four non-negotiable scheduling invariants:

#### Damper 1: Quantum Reclaim Gate ($Q_{min}$)
The scheduler is strictly forbidden from executing micro-prunes for trivial savings:
- **Quantum Floor**: $Q_{min} = \max(4,000\text{ tokens}, 0.05 \times W_{window})$.
- **Invariant**: If the total sum of reclaimable tokens across all eligible cold observations is less than $Q_{min}$, the prune pass **aborts as a strict No-Op**.
- **Rationale**: Never sacrifice a hot prefix cache of 50,000 tokens to reclaim a 300-token scrap.

#### Damper 2: Dual-Band Hysteresis Loop ($W_{high} \to W_{low}$)
Pruning operates on a hysteresis loop, never a single point threshold:
- **Activation Boundary**: $W_{high} = 0.70 \times W_{window}$.
- **Target Landing Boundary**: $W_{low} = 0.50 \times W_{window}$.
- **Hysteresis Rule**: A budget-driven prune executes only when the plan can recover enough tokens to pull the session from $W_{high}$ down toward $W_{low}$.
- **Protected Cruise Zone ($70\% \to 85\%$)**: If cold prunable observations cannot achieve this drop, the system permits the session to enter the Cruise Zone. Across this entire band, **pruning is completely deactivated**, granting the session a prolonged sequence of 100% KV-cache hit turns.

#### Damper 3: Hot-Tail Quarantine (L1 Working Set Protection)
The scheduler enforces an absolute quarantine around recent context:
- **Quarantine Ceiling**: $\text{Tail}_{\text{protect}} = \max(16,000\text{ tokens}, 3\text{ complete rounds})$.
- **Invariant**: Any observation created within $\text{Tail}_{\text{protect}}$ is strictly immune from budget-driven pruning, regardless of total session pressure.
- **Rationale**: Active reasoning chains and immediate tool references must never be disturbed while under active model attention.

#### Damper 4: Unidirectional Exhaustion Circuit-Breaker
Pruning is a resource with an exhaustion curve:
- Because retired observations become immutable tombstones (`[cleared tool result ...]`), they cannot yield tokens twice.
- When cold prunable observations are exhausted (or yield $< Q_{min}$), the scheduler trips an internal register:
  ```rust
  session_state.pruning_exhausted = true;
  ```
- **Short-Circuit**: When `pruning_exhausted == true`, subsequent turns **completely bypass the pruning planner**. Zero CPU scanning, zero token recalculation, and absolute zero risk of thrashing.
- The session cruises cleanly to $W_{compact}$ (85%), where Compaction performs a single macro-reset and clears the exhaustion flag.

---

### 2. Generative Horizon Compaction (Epoch Savepoint)

When total context crosses the hard watermark ($W_{compact} = 0.85 \times W_{window}$), pruning cannot resolve the pressure because non-tool dialogue dominates the prompt. The scheduler initiates a **Generative Horizon Compaction**:

#### 1. Fundamental Distinction from Claim-Checks
- **Tool Observation (ADR-0282)**: 1:1, mechanical, lossless, deterministic claim-check backed by CAS.
- **Horizon Compaction**: 1:N, cognitive, lossy, irreversible semantic state synthesis produced by an LLM Summarizer.

#### 2. The Typed `BeliefState` Checkpoint
Compaction does not emit an unstructured narrative. It executes an isolated summarization sub-pass that extracts a structured `BeliefState`:

```rust
pub struct BeliefState {
    /// The high-level intent, constraints, and success criteria of the session.
    pub task_objective: String,
    /// Verified facts, accepted architectural decisions, and confirmed findings.
    pub established_facts: Vec<String>,
    /// Current environment posture (files modified, active branch, running daemons).
    pub system_posture: SystemPosture,
    /// Unfinished tasks, active hypotheses, and immediate next steps.
    pub pending_plan: Vec<String>,
}
```

#### 3. View Commit and Epoch Re-Rooting ([ADR-0278](0278-compaction-as-a-view-commit.md))
- The scheduler sets the Session IR `compaction_horizon` pointer to the latest safe boundary.
- Nodes strictly prior to `compaction_horizon` are sealed into historical archive.
- The `BeliefState` is committed as an atomic `NodePayload::Compaction` node at the root of the active lineage.
- Active context immediately drops from $85\%$ to $W_{target} = 25\%$.
- The exhaustion flag is reset (`pruning_exhausted = false`), inaugurating a clean, healthy new operational epoch.

---

## Invariants & Behavioral Boundaries

- **`[INV-SCHED-01]` Zero-Thrashing Quantum Floor**: Budget-driven pruning MUST NOT execute if total planned token recovery is less than $Q_{min}$ (4,000 tokens). Micro-pruning on scrap yields is a fatal invariant violation.
- **`[INV-SCHED-02]` Dual-Band Hysteresis**: The context planner MUST honor the protected cruise zone between $W_{high}$ (70%) and $W_{compact}$ (85%). Pruning must not fire within this band unless triggered by an event-driven causal invalidation (ADR-0282).
- **`[INV-SCHED-03]` Hot-Tail Quarantine**: No observation within the tail protection window (16,000 tokens / 3 rounds) may be cleared by budget-driven pruning under any circumstances.
- **`[INV-SCHED-04]` Circuit-Breaker Short-Circuit**: Once cold reclaimable yields drop below $Q_{min}$, `pruning_exhausted` must be set to `true`, and all subsequent budget pruning sweeps must be bypassed until the next epoch compaction.
- **`[INV-SCHED-05]` Atomic Horizon View Commit**: Compaction must follow ADR-0278: summarization occurs out-of-band; commit to the active branch is atomic; failure of the summarizer rolls back cleanly without dropping active dialogue nodes.

---

## Negative Knowledge: Rejected Alternatives (`[INV-AGENT-01]`)

### 1. Point-Threshold Greedy Pruning
- **Approach**: Firing pruning whenever token count exceeds 65%, clearing whatever outputs are available.
- **Why Failed**: Generated severe cache thrashing. Sessions with 30+ turns experienced cache misses on 100% of turns, inflating prompt prefill latency by 800% and multiplying API costs.

### 2. Treating Compaction as an "Addressable Claim-Check"
- **Approach**: Formatting compaction checkpoints with a claim-check handle `fold:<id>` and expecting the model to "demand-page" folded turns back into context.
- **Why Failed**: A folded dialogue horizon consists of tens of thousands of tokens. If the model paged it back in, the prompt immediately breached the context window, triggering an infinite recursive compaction loop. Compaction is a state transition, not an archival page cache.

### 3. Continuous Turn-by-Turn Summarization
- **Approach**: Running an LLM summarizer on every single turn to maintain a rolling summary.
- **Why Failed**: Massive cost inflation (an LLM invocation per turn), introduced high end-to-end latency, and caused cognitive drift where nuanced instructions were distorted through repeated lossy summarization telephone games.

### 4. Pruning Natural Dialogue (User/Assistant Nodes) via Heuristics
- **Approach**: Dropping early User or Assistant messages using text truncation heuristics rather than running full compaction.
- **Why Failed**: Severed conversational causality. The assistant forgot the user's explicit negative constraints ("never touch production DB"), causing severe safety violations.

---

## Consequences

### Positive
- **Guaranteed Cache Stability**: The protected cruise zone (70%–85%) guarantees that long sessions run with near-100% KV-cache hit rates during their most computationally intensive stages.
- **Zero Jitter / Zero Thrashing**: The combination of $Q_{min}$, Hysteresis, and the Exhaustion Circuit-Breaker mathematically eliminates boundary oscillations.
- **Infinite Lifecycle Scalability**: Generative Horizon Compaction compresses belief state cleanly, allowing sessions to run for hundreds of turns without hitting token limits.
- **Strict Separation of Concerns**: Mechanical data-plane GC (Prune) is completely decoupled from cognitive state synthesis (Compact).

### Negative / Trade-Offs
- **Compaction Latency Spike**: When 85% is reached, the single epoch compaction turn incurs an out-of-band LLM summarization call (3–8 seconds), after which the session resumes at light speed.
- **Temporary Memory Inflation**: Allowing context to cruise from 70% to 85% without micro-pruning consumes more prompt memory than aggressive thrashing, but saves orders of magnitude in compute latency and cost.

---

## References

- [ADR-0241: Session IR Causal Graph and Compiler Pipeline](0241-session-ir-causal-graph-and-compiler-pipeline.md)
- [ADR-0277: Unified Context Planner and Request Compiler](0277-unified-context-planner-and-request-compiler.md)
- [ADR-0278: Compaction as a View Commit](0278-compaction-as-a-view-commit.md)
- [ADR-0280: Versioned Context Policy and Clean-Break Cutover](0280-versioned-context-policy-and-clean-break-cutover.md)
- [ADR-0282: Tool Observation Claim-Check Lifecycle and Content-Addressed Backing Store](0282-tool-observation-claim-check-lifecycle-and-cas-backing-store.md)
