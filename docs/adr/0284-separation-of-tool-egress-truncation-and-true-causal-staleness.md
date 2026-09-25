---
id: ADR-0284
title: "Separation of Tool Egress Truncation, Eradication of Pre-Round Speculative Mutation, and Anti-Thrashing Context Scheduling"
status: accepted
date: 2026-10-26
scope: contracts/pressure, contracts/session_ir, agent/orchestration
superseded_by: null
negative_knowledge: true
---

# 0284. Separation of Tool Egress Truncation, Eradication of Pre-Round Speculative Mutation, and Anti-Thrashing Context Scheduling

- **Status:** Accepted
- **Date:** 2026-10-26
- **Scope:** `muta-contracts` (pressure, staleness, observation lifecycle), `muta-agent` (orchestrator, round lifecycle), `muta-runtime`
- **Deciders:** Muta Architecture Team
- **Builds on:** [ADR-0241](0241-session-ir-causal-graph-and-compiler-pipeline.md) (Session IR causal graph), [ADR-0275](0275-immutable-execution-facts-and-branch-local-context-views.md) (Immutable execution facts), [ADR-0277](0277-unified-context-planner-and-request-compiler.md) (Unified context planner and request compiler), [ADR-0280](0280-versioned-context-policy-and-clean-break-cutover.md) (Versioned context policy and clean-break cutover), [ADR-0282](0282-tool-observation-claim-check-lifecycle-and-cas-backing-store.md) (Tool observation claim-check lifecycle), [ADR-0283](0283-anti-thrashing-context-scheduling-and-generative-horizon-compaction.md) (Anti-thrashing context scheduling)
- **Supersedes / Eradicates:** Legacy `near_tail_stale` pre-round check from [ADR-0254](0254-epistemic-observation-lifecycle-ingestion-folding-and-near-tail-invalidation.md) §3 and `has_stale_tool_results` heuristic.

---

## Context and Problem Statement

In autonomous multi-turn agent sessions, two systemic architectural conflations caused confusing visual context meter jumps and severe prompt cache thrashing:

1. **Category Conflation between Tool Egress Truncation and Context Compression**:
   When a tool executes (e.g. `run_command`, `read_text`, `search_text`), physical egress bounding occurs *before* inserting the result into a dialogue `Message`. Large payloads are spooled to Content-Addressed Storage (CAS), while a bounded preview with an epistemic claim-check handle (`call:<call_id>`) is returned. This is physical admission bounding, **not a context lifecycle compression pass**. What is committed to `round_history` is already bounded and represents the active request projection.
2. **The `near_tail_stale` Legacy Pathology**:
   In [ADR-0254](0254-epistemic-observation-lifecycle-ingestion-folding-and-near-tail-invalidation.md) §3, an ad-hoc speculative pre-round check (`near_tail_stale = has_stale_tool_results(...)`) was introduced to proactively clear old build outputs before a new round started.
   This mechanism suffered from fatal conceptual and practical flaws:
   - **False Staleness**: `has_stale_tool_results` was implemented as `!plan_prune(...).is_empty()`. Because any cold tool output older than 6,000 tokens entered the degradation plan, `near_tail_stale` fired unconditionally on every single round boundary.
   - **Premature History Mutation**: Even when a 200,000-token model had only consumed 15,000 tokens (7.5% capacity), the orchestrator speculatively ran `prune_and_commit`, mutating intermediate messages into placeholders (`[cleared tool result: ...]`).
   - **Cache Thrashing (Busting the Hot Prefix)**: Mutating intermediate messages destroyed the provider-side KV-cache prefix (Anthropic / OpenAI / DeepSeek prompt caches), turning 95%+ cached turns into expensive full-prefill re-evaluations and spiking TTFT latency.
   - **Visual Token Jump ("Surprise Compression")**: The terminal model bar context meter displayed high token usage at the conclusion of Round N, but suddenly collapsed at the start of Round N+1 when the user pressed Enter.

---

## Decision

We execute a **clean-break eradication** of all speculative pre-round history mutations and decouple physical tool egress truncation from context lifecycle passes:

```text
┌────────────────────────────────────────────────────────────────────────┐
│ Phase 1: Tool Execution & Egress Truncation (Physical Admission Limit) │
│ - Output > limit -> Spool raw bytes to CAS; emit bounded preview with │
│   epistemic handle `call:<call_id>`.                                   │
│ - Result is immutable fact; NO context compression pass is involved. │
└────────────────────────────────────────────────────────────────────────┘
                                 │
                                 ▼
┌────────────────────────────────────────────────────────────────────────┐
│ Phase 2: Zero Speculative Pre-Round Mutation (Protected Cruise Zone)   │
│ - In the Cruise Zone (0% -> 70% W_high), NO PRUNING OCCURS AT ALL.    │
│ - `near_tail_stale` and `has_stale_tool_results` are completely       │
│   eradicated from orchestrator and contracts.                          │
│ - History is 100% byte-stable across rounds; KV-cache stays hot.       │
└────────────────────────────────────────────────────────────────────────┘
                                 │
                                 ▼
┌────────────────────────────────────────────────────────────────────────┐
│ Phase 3: Anti-Thrashing Budget Pruning (ADR-0283 High Watermark)        │
│ - Pruning engages ONLY when request_tokens > prune_threshold_tokens.   │
│ - Always enforces Quantum Reclaim Floor (Q_min >= 4,000 tokens).       │
│ - Eliminates micro-pruning scrap yields and cache jitter.              │
└────────────────────────────────────────────────────────────────────────┘
```

### 1. Complete Eradication of `near_tail_stale`
In `execute_round`, the dual condition `under_pressure || near_tail_stale` is eliminated. Pruning is governed strictly and solely by window pressure breaching the high watermark:

```rust
// ADR-0283 / ADR-0284: Budget-driven pruning engages strictly when window pressure
// breaches the prune threshold watermark. In the healthy cruise zone, history mutation
// is completely deactivated to preserve 100% KV-cache prefix stability and eliminate
// artificial context size drops across round boundaries.
if projection.prune
    && request_estimate.total_tokens > projection.budget.prune_threshold_tokens
{
    prune_and_commit(
        &mut round_history,
        &session,
        &projection,
        agent.token_weights_handle(),
    )
    .await?;
    request_estimate = estimate_off_executor(&agent, &round_history).await;
}
```

### 2. Elimination of Micro-Pruning Scrap Yields
In `prune_and_commit`, the legacy branch that dropped `min_reclaim` to 100 tokens is eradicated. The quantum reclaim gate ($Q_{min} \ge 4,000$ tokens, [ADR-0283](0283-anti-thrashing-context-scheduling-and-generative-horizon-compaction.md) Damper 1) is universally enforced.

### 3. Deletion of `has_stale_tool_results`
The flawed heuristic `has_stale_tool_results` is removed from `muta-contracts` and `muta-agent`. Causal supersession order is handled naturally inside `plan_prune` when budget pressure triggers a legitimate prune sweep.

---

## Invariants & Behavioral Boundaries

- **`[INV-EGRESS-01]` Tool Egress Claim-Check Invariance**: Tool output truncation at ingestion time is an admission boundary control, never a context lifecycle compression event. Once emitted, the bounded message is an immutable fact.
- **`[INV-CRUISE-01]` Zero Speculative Mutation**: In the sub-threshold cruise zone ($< W_{high}$), the orchestrator MUST NOT mutate historical messages across round boundaries under any circumstances.
- **`[INV-SCHED-01]` Quantum Reclaim Floor**: Pruning passes must recover at least $Q_{min}$ (4,000 tokens). Micro-pruning on scrap yields is forbidden.

---

## Negative Knowledge: Rejected Alternatives (`[INV-AGENT-01]`)

### 1. Retaining `near_tail_stale` with Finer-Grained Staleness Flags
- **Approach**: Attempting to salvage `near_tail_stale` by distinguishing between true causal invalidation and unreferenced tools.
- **Why Failed**: When context utilization is low (e.g. 10%), mutating history to erase an old test log provides zero cognitive benefit to the model while completely destroying the provider prompt cache (causing 5–15s TTFT latency and 10x cost inflation). Speculative pre-round pruning is an architectural anti-pattern.

### 2. Micro-Pruning with Reduced Thresholds (`min_reclaim = 100`)
- **Approach**: Allowing pruning to run if even 100 tokens can be reclaimed.
- **Why Failed**: Triggered severe cache thrashing (ADR-0283) where every round mutated intermediate messages, completely negating prompt caching for trivial token savings.

### 3. Conflating Degradation Plans with Staleness (`has_stale_tool_results == !plan_prune().is_empty()`)
- **Approach**: Using the non-emptiness of the general budget degradation plan as a proxy for "stale tool results".
- **Why Failed**: Coupled capacity degradation with causal invalidation, leading to spurious history rewrites.

---

## Consequences

### Positive
- **Zero Visual Jitter**: The context meter in the terminal bottom-left does not experience sudden, arbitrary downward jumps across round transitions.
- **100% KV-Cache Prefix Stability**: Sessions in the cruise zone (0%–70%) maintain identical historical prefixes, maximizing cache hits and minimizing latency and token costs.
- **Radical Code Simplification**: Eradicated complex, buggy staleness check branches and duplicate evaluation modes.

### Negative / Trade-Offs
- Superseded test logs or file reads remain in context while total tokens are well within budget ($< 70\%$). However, modern foundation models with 200k+ context windows handle this effortlessly, and retaining previous attempt logs frequently aids troubleshooting continuity.

---

## References

- [ADR-0275: Immutable Execution Facts and Branch-Local Context Views](0275-immutable-execution-facts-and-branch-local-context-views.md)
- [ADR-0277: Unified Context Planner and Request Compiler](0277-unified-context-planner-and-request-compiler.md)
- [ADR-0280: Versioned Context Policy and Clean-Break Cutover](0280-versioned-context-policy-and-clean-break-cutover.md)
- [ADR-0282: Tool Observation Claim-Check Lifecycle and Content-Addressed Backing Store](0282-tool-observation-claim-check-lifecycle-and-cas-backing-store.md)
- [ADR-0283: Anti-Thrashing Context Scheduling and Generative Horizon Compaction](0283-anti-thrashing-context-scheduling-and-generative-horizon-compaction.md)
