---
id: ADR-0282
title: "Tool Observation Claim-Check Lifecycle and Content-Addressed Backing Store"
status: proposed
date: 2026-10-25
scope: persistence/blobs, contracts/session_ir, runtime/agent, tools/inspect
superseded_by: null
negative_knowledge: true
---

# 0282. Tool Observation Claim-Check Lifecycle and Content-Addressed Backing Store

- **Status:** Proposed
- **Date:** 2026-10-25
- **Scope:** `muta-persistence` (CAS blobs, observation indices), `muta-contracts` (Session IR, observation lifecycle), `muta-agent` (tool runners, admission control, causal invalidation), `muta-runtime` (inspect channel)
- **Deciders:** Muta Architecture Team
- **Builds on:** [ADR-0241](0241-session-ir-causal-graph-and-compiler-pipeline.md) (Session IR causal graph), [ADR-0262](0262-demand-paged-epistemic-memory-and-unified-inspect-channel.md) (Demand-paged epistemic memory and unified inspect channel), [ADR-0275](0275-immutable-execution-facts-and-branch-local-context-views.md) (Immutable execution facts), [ADR-0276](0276-bounded-raw-artifact-capture-and-durable-publication.md) (Bounded raw artifact capture and durable publication)
- **Amends:** [ADR-0254](0254-epistemic-observation-lifecycle-ingestion-folding-and-near-tail-invalidation.md) (Epistemic observation lifecycle)
- **Sibling decisions:** [ADR-0283](0283-anti-thrashing-context-scheduling-and-generative-horizon-compaction.md) (Anti-thrashing context scheduling and generative horizon compaction)

---

## Context and Problem Statement

In autonomous multi-turn agent systems, tool execution results (terminal stdout/stderr, file reads, LSP symbol queries, web reader dumps) dominate active context consumption, frequently occupying 70% to 90% of total tokens.

Historically, systems have conflated two separate concerns into unprincipled, ad-hoc string slicing:
1. **Admission Truncation**: Cutting an oversized tool output as soon as it arrives to fit within the prompt window.
2. **Context Pruning**: Evicting or blanking stale tool outputs from past turns to free up tokens when the conversation grows long.

This conflation introduced three major architectural failures:
- **Epistemic Amnesia (Cognitive Black Hole)**: Once an output was truncated or pruned, the omitted raw bytes were permanently lost from the model surface. When subsequent debugging required viewing a specific line or compiler stack trace, the model was forced to re-read files or re-run commands, producing **Information Hoarding** behavior.
- **Invoice Identifier Drift**: Different pipeline passes formatted disparate, unstructured placeholders (e.g. `[Output truncated]`, `[Result cleared]`), losing the link between the truncated slice and the original execution call.
- **Entanglement of Staleness and Budget Pressure**: Systems only pruned tool outputs when total context crossed a global token threshold. Consequently, objectively stale and invalidated data (e.g., outdated file reads superseded by later edits, or obsolete compiler errors superseded by passing tests) remained inside the active context, confusing the model and inducing severe hallucination.

A clean-break, uncompromised architecture requires recognizing that **Truncate and Prune are not distinct operations—they are merely two lifecycle phases of the exact same Claim-Check (发票 / 存根) mechanism**, backed by an immutable Content-Addressed Storage (CAS) blob engine.

---

## Decision

We formally elevate tool execution facts into first-class **Tool Observations** governed by a unified **Claim-Check Lifecycle State Machine**, backed by immutable Content-Addressed Storage (CAS).

```text
               Tool Execution Complete (Raw Bytes)
                              │
                              ▼
                [ Step 0: Durable Publication ]
           Write raw bytes to CAS under SHA-256 hash
           Register invoice `call:<call_id>` in SQLite
                              │
                              ▼
                   [ Phase 1: Ingestion Gate ]
                  Is payload > Admission Cap?
                   ├── No  ──► [ State 1: Raw / In-Budget ]
                   └── Yes ──► [ State 2: Active Truncated ]
                               (Head/Tail preview + Invoice `call:<id>`)
                                      │
                                      ▼
                        [ Phase 2: Invalidation Gate ]
                 Causal event occurs (e.g. File Edit / Test Pass)
                                      │
                                      ▼
                           [ State 3: Retired Pruned ]
                     (Body cleared to Tombstone + Invariant Invoice `call:<id>`)
                                      │
                                      ▼
                    [ Phase 3: Demand Paging (Inspect) ]
             Model presents invoice `call:<id>` to `inspect` tool
           CAS streams lossless requested range into current turn
```

---

### 1. The Observation Lifecycle State Machine

A tool execution result transitions through three strictly defined states:

```rust
pub enum ObservationLifecycle {
    /// State 1: Raw payload retained in working context (within token admission budget).
    Raw,

    /// State 2: Active Truncated (oversized at ingestion).
    /// Retains a bounded head and tail preview with elision metrics, plus the invariant invoice handle.
    ActiveTruncated {
        head_preview: String,
        tail_preview: String,
        retained_tokens: usize,
        omitted_bytes: usize,
    },

    /// State 3: Retired Pruned (causally superseded or garbage-collected).
    /// The payload body is entirely cleared to a lightweight tombstone, preserving causal intent and the invoice.
    Retired {
        reason: InvalidationReason,
        original_tokens: usize,
        original_lines: usize,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InvalidationReason {
    /// File read invalidated by a subsequent write/edit to the same resource.
    SupersededByMutation { target_resource: String, mutator_node_id: String },
    /// Diagnostic/test execution invalidated by a subsequent run.
    SupersededByExecution { successor_node_id: String },
    /// Reclaimed under quantum budget pressure after recency quarantine expiration.
    BudgetRelief,
}
```

#### Core Lifecycle Invariants
1. **The Claim-Check Invariant**: Across both `ActiveTruncated` and `Retired`, the addressing handle **`call:<call_id>` NEVER changes**. It is assigned at execution time and remains immutable for the lifetime of the session.
2. **Deterministic Losslessness**: Regardless of lifecycle transitions, the original, un-truncated, un-folded raw byte sequence resides immutably in CAS.

---

### 2. End-to-End Three-Plane Architecture Mapping

| Layer | State 1: Raw | State 2: Active Truncated (Ingestion) | State 3: Retired Pruned (Invalidation) |
| :--- | :--- | :--- | :--- |
| **1. Persistence Plane** (`muta-persistence`) | Raw bytes written to CAS: `blobs/ab/cdef...` (`hash = SHA256(bytes)`). SQLite records `state: 'active'`. | Same CAS blob; SQLite updates preview slice coordinates and token metrics. | **Zero change to CAS blob**. SQLite updates `state: 'retired'`, `retired_reason: 'superseded'`. |
| **2. Session IR Plane** (`muta-contracts`) | `NodePayload::Observation { lifecycle: Raw, .. }` | `NodePayload::Observation { lifecycle: ActiveTruncated { .. }, .. }` | Node remains in causal graph! `lifecycle` transitions to `Retired { reason, .. }`. |
| **3. Wire Request Plane** (`muta-llm-client`) | Full tool output emitted in message content. | Emits head/tail preview with invoice string: `... [N tokens elided — inspect "call:<id>"] ...`. **Cache hot from turn 2 onward**. | Emits tombstone receipt: `[cleared tool result: read_text(foo.rs) — superseded by edit at turn 6 — inspect "call:<id>"]`. |

---

### 3. Event-Driven Causal Invalidation (Decoupled from Token Watermarks)

Invalidation is an **objective causal property of the execution graph**, not an opportunistic token-saving trick:

- **Immediate Event-Driven Transition**: When an `edit_text` or `write_file` operation succeeds, the causal engine immediately iterates prior active `read_text` observations referencing that file URI and transitions them to `ObservationLifecycle::Retired`.
- **Immediate Diagnostic Supersession**: When a test/build tool completes, preceding failure outputs of the same scope are transitioned to `Retired`.
- **Why This Matters**: Outdated information is removed from active context *before* the model plans its next step. The model never reasons over stale code or superseded compiler errors, eliminating context pollution at its root cause.

---

### 4. Demand Paging via the Unified Inspect Channel

When the model requires granular inspection of an observation in `ActiveTruncated` or `Retired` state:

1. **Protocol Invariant**: The model calls `inspect(handle: "call:<call_id>", offset?: N, limit?: M, pattern?: "regex")`.
2. **Execution Flow**:
   - The inspect service maps `call_id` to the CAS `blob_hash`.
   - The blob store opens the immutable file via lock-free reader (`O_RDONLY`).
   - Slices lines or matches regex patterns within a strict token budget (default 4,096 tokens).
   - Returns the fresh slice as a standard `ToolResult` in the current turn.
3. **No Graph Mutation**: Invoking `inspect` does not "un-prune" historical nodes or rewrite prior turns. The retrieved data enters the dialogue as a current observation, preserving historical DAG immutability and KV-Cache prefixes.

---

## Invariants & Behavioral Boundaries

- **`[INV-OBS-01]` Publish-Before-Reference Durability**: No tool observation may be projected into Session IR or emitted over the wire until its raw payload has been durably fsynced into the Content-Addressed Storage (CAS) or write-ahead log.
- **`[INV-OBS-02]` Handle Immutability**: The claim-check invoice for a tool observation is strictly derived from its execution `call_id` (`call:<call_id>`). It must remain invariant across all lifecycle state transitions (`Raw` $\to$ `ActiveTruncated` $\to$ `Retired`).
- **`[INV-OBS-03]` Event-Driven Invalidation Independence**: Causal invalidation (file mutation, execution supersession) MUST execute as an immediate reaction to causal graph events, without evaluating or gating on token capacity watermarks.
- **`[INV-OBS-04]` Inspect Channel Orthogonality**: The `inspect` channel is strictly a read-only projection mechanism. It must never mutate prior node states, resurrect cleared graph payloads, or graft history.
- **`[INV-OBS-05]` Protocol ID Integrity**: Lowering an observation to provider wire shapes (OpenAI, Anthropic, Gemini) MUST retain the exact original `tool_call_id`. Fabricating placeholder IDs or converting tool results into arbitrary system messages is strictly prohibited.

---

## Negative Knowledge: Rejected Alternatives (`[INV-AGENT-01]`)

### 1. In-Memory Ephemeral Truncation
- **Approach**: Truncating tool outputs directly in memory without writing the raw bytes to CAS storage.
- **Why Failed**: When the model subsequently needed details from the truncated section (e.g. specific line numbers, compiler error warnings), the data was unrecoverable. Models responded by repeatedly re-running expensive commands or refusing to proceed.

### 2. Disparate Truncate and Prune Schemas
- **Approach**: Treating Truncate as a tool-runner text filter and Prune as a message-array cleaner, formatting arbitrary strings like `[Output truncated]` and `[Tool result cleared]`.
- **Why Failed**: Lacked a unified addressing format. Downstream components could not tell whether an elided block was recoverable or where it lived. Models could not deterministically formulate `inspect` invocations.

### 3. Gating Invalidation on Token Pressure
- **Approach**: Delaying the clearing of superseded file reads or old compiler errors until total session tokens crossed 65% capacity.
- **Why Failed**: Left actively misleading, false information in context during early turns. The model hallucinated that bugs already fixed in turn 4 were still present because turn 2's error log was still sitting in working memory.

### 4. Re-assigning New Unique Identifiers upon Pruning
- **Approach**: Creating a new tombstone identifier `pruned_call_<uuid>` when transitioning an observation to the retired state.
- **Why Failed**: Severed the relationship between the tool call and its result, causing strict protocol validators (OpenAI strict mode) to fail with missing `tool_call_id` reconciliation errors.

---

## Consequences

### Positive
- **Zero Epistemic Amnesia**: All historical execution evidence is permanently preserved in CAS and demand-pageable via `inspect`.
- **Unified Tool Data Pipeline**: Truncate and Prune are unified under a single, typed state machine (`ObservationLifecycle`).
- **Hallucination Suppression**: Stale execution results are immediately purged upon causal supersession, ensuring the model only observes ground truth.
- **Storage Efficiency**: CAS storage deduplicates repeated reads of identical files or redundant test outputs automatically across sessions.

### Negative / Trade-Offs
- **Disk Footprint**: Raw execution artifacts must be held in CAS until session tombstone garbage collection cleans unreferenced blobs.
- **One Roundtrip for Offstream Retrieval**: If the model needs elided lines, it must perform one `inspect` call. (This is vastly preferable to holding millions of dead tokens in context).

---

## References

- [ADR-0241: Session IR Causal Graph and Compiler Pipeline](0241-session-ir-causal-graph-and-compiler-pipeline.md)
- [ADR-0262: Demand-Paged Epistemic Memory and Unified Inspect Channel](0262-demand-paged-epistemic-memory-and-unified-inspect-channel.md)
- [ADR-0275: Immutable Execution Facts and Branch-Local Context Views](0275-immutable-execution-facts-and-branch-local-context-views.md)
- [ADR-0276: Bounded Raw Artifact Capture and Durable Publication](0276-bounded-raw-artifact-capture-and-durable-publication.md)
- [ADR-0283: Anti-Thrashing Context Scheduling and Generative Horizon Compaction](0283-anti-thrashing-context-scheduling-and-generative-horizon-compaction.md)
