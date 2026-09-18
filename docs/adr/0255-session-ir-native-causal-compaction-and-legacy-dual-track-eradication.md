# 0255. Session IR Native Causal Compaction and Legacy Dual-Track Eradication

- **Status:** Proposed
- **Date:** 2026-10-20
- **Scope:** `core/contracts`, `core/runtime`, `muta-agent`, `muta-contracts`, `muta-persistence`
- **Deciders:** Muta Architecture Team
- **Builds on:** [ADR-0241](0241-session-ir-causal-graph-and-compiler-pipeline.md) (Session IR causal graph and compiler), [ADR-0249](0249-session-ir-native-execution-runtime-and-durable-suspension.md) (Session IR native runtime), [ADR-0251](0251-causal-dag-branching-timelines-and-unified-aside-multiverse.md) (Causal DAG branching timelines), [ADR-0254](0254-epistemic-observation-lifecycle-ingestion-folding-and-near-tail-invalidation.md) (Epistemic observation folding)
- **Supersedes:** [ADR-0019](0019-model-relative-context-compaction.md) (model-relative transcript compaction)
- **Amends:** [ADR-0186](0186-single-transcript-projection-directives-persistence.md) (retiring compaction projection directives in favor of causal horizons)

---

## Context and Problem Statement

Following the ratification of [ADR-0241](0241-session-ir-causal-graph-and-compiler-pipeline.md) and [ADR-0249](0249-session-ir-native-execution-runtime-and-durable-suspension.md), `SessionIR` was established as the platform-agnostic, single source of truth for agent dialogues. However, the runtime compaction subsystem remains mired in a severe **architectural dual-track split**:

1. **The Legacy Pipeline Impedance Mismatch**:
   Runtime context reduction still executes through the legacy `Transcript` pipeline: `session.model_window()` $\to$ `select_compaction_for_target` $\to$ `summarize_with_provider` $\to$ `commit_context_projection` (`ProjectionDirective::Compact`). This forces persistence through a deprecated bridge (`ir_bridge.rs`) and bypasses the `SessionIR` causal graph entirely.

2. **KV Prompt Cache Busting & Latency Spikes**:
   Legacy compaction prepends a volatile `[Conversation checkpoint]` text header to the projected transcript window. On modern frontier models (Claude 3.7, GPT-4.5, DeepSeek), altering the root prefix shatters the KV-cache, turning $85\text{--}90\%$ cached discount requests into full recomputations with 5–15 second Time-to-First-Token (TTFT) latency penalties.

3. **Multi-Turn Cognitive Degradation ("Amnesia Loop")**:
   Legacy summarization produces a single prose narrative and forcibly slices it with tokenizer-level byte cuts (`truncate_summary_to_token_budget`). After 2–3 successive compaction cycles, initial user task constraints, technical invariants, and file edit states are completely dissolved by model hallucination and token-boundary slicing.

4. **Orphaned Advanced Modules**:
   Modules introduced in `crates/muta-agent/src/compaction/` (`heuristic.rs`, `split_compaction.rs`, `file_tracker.rs`, `observation_folding.rs`) were built against the transitional `SessionTree` abstraction and remain completely uninvoked by the primary execution loop in `orchestration.rs`.

A clean-break, uncompromised compaction engine is required to unify compaction natively within `SessionIR`, eliminating legacy dual-track bridges while absorbing the premier industry practices of intent retention and prefix-cached compaction.

---

## Decision Drivers

- **Zero Legacy Burden (Clean Break)**: Eradicate `Transcript`, `SessionTree`, `ProjectionDirective::Compact`, and `ir_bridge.rs`. Establish `SessionIR` as the sole substrate.
- **Topological Causal Compaction**: Represent context reduction as immutable causal node creation and horizon advancement on `CausalGraph`, retaining full history auditability and DAG branching.
- **Strict Prompt Cache (KV-Cache) Invariance**: Maintain an immutable static system and workspace prefix across compactions. Isolate compaction checkpoints to stable pre-tail horizons.
- **Uncompromised Intent & Artifact Fidelity**: Retain the complete user prompt lineage and programmatic file-touch manifests (deterministic `FileTracker`), completely eliminating hallucinated summary drift.
- **No Tokenizer腰斩 (Zero Hard-Truncation)**: Enforce strict token limits via model generation constraints and structured YAML/Markdown schemas, banning destructive string slicing.
- **Autonomous Self-Healing**: Handle compaction prompt context-overflow reactively without terminal agent deadlock.

---

## Decision Outcome

We establish the **Session IR Native Causal Compaction Architecture (CCA)** across `muta-contracts`, `muta-agent`, `muta-persistence`, and `muta-runtime`.

```text
┌─────────────────────────────────────────────────────────────────────────────┐
│ 1. Autonomous Decision Engine (Heuristic + Budget)                          │
│    • Milestone Gates: TodosSettled, TopicShift (Semantic transition)        │
│    • Budget Gates: Soft Threshold (80% window) + Provider ContextOverflow   │
└──────────────────────────────────────┬──────────────────────────────────────┘
                                       │ Trigger Event
┌──────────────────────────────────────▼──────────────────────────────────────┐
│ 2. Atomic Causal Lineage Splitter (split_active_lineage)                    │
│    • Linear lineage walkback from state.active_leaf                         │
│    • Turn-internal fine slicing with ToolCall <-> ToolResult atomic lock     │
│    • Splits lineage into: [Nodes to Fold] vs. [Preserved Volatile Tail]     │
└──────────────────────────────────────┬──────────────────────────────────────┘
                                       │ (Fold Candidates, Tail)
┌──────────────────────────────────────▼──────────────────────────────────────┐
│ 3. Deterministic Artifact Extraction & Incremental Rollup                   │
│    • AST/Tool extraction: Programmatic FileTracker (Read/Created/Modified)  │
│    • Intent Chain extraction: Preserves historical User Prompts             │
│    • Rollup Engine: Merges prior summary + new delta into single schema     │
│    • Generation Bounded: max_tokens ceiling, zero post-generation slicing   │
└──────────────────────────────────────┬──────────────────────────────────────┘
                                       │ CompactionSummaryPayload
┌──────────────────────────────────────▼──────────────────────────────────────┐
│ 4. Causal Graph Mutation & State Horizon Advancement                        │
│    • Insert CausalNode(NodePayload::Compaction) pointing to split_parent    │
│    • Advance state.compaction_horizon to the new Compaction Node            │
│    • Re-parent Preserved Tail onto the new Compaction Node                  │
└──────────────────────────────────────┬──────────────────────────────────────┘
                                       │ SessionIR Mutated
┌──────────────────────────────────────▼──────────────────────────────────────┐
│ 5. Four-Pass Cache-Stabilizing Compiler (ADR-0241 Alignment)                │
│    • Pass 1: Lineage walk stops at state.compaction_horizon (O(tail))       │
│    • Pass 2: Layout: [Static System/WorldState] -> [Compaction + User Prompts]│
│                     -> [Preserved Volatile Tail]                            │
│    • Pass 3: Computes deterministic SHA256 prefix_fingerprint for KV Cache   │
│    • Pass 4: Lowers to ModelRequest with explicit prompt_cache_preference   │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

### 1. Causal Graph Representation (`NodePayload::Compaction`)

In `crates/muta-contracts/src/session_ir/types.rs`, compaction is modeled as a first-class node payload:

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactionSummaryPayload {
    /// Overarching user objective and current active phase
    pub objective: String,
    /// Critical architectural constraints, dependency requirements, and paths
    pub architectural_constraints: Vec<String>,
    /// Completed and verified milestones
    pub completed_work: Vec<String>,
    /// Work active or blocked right at the compaction horizon
    pub in_flight_state: String,
    /// Deterministically extracted file touches (Path -> TouchKind)
    pub tracked_files: HashMap<String, FileTouchKind>,
    /// Correlated previous compaction node ID (incremental rollup chain)
    pub prior_compaction_node: Option<NodeId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FileTouchKind {
    Read,
    Created,
    Modified,
    Deleted,
}
```

`SessionState` gains an authoritative horizon pointer:
```rust
pub struct SessionState {
    pub active_leaf: Option<NodeId>,
    /// Nodes strictly prior to this pointer are folded; compiler halts backward walk here.
    pub compaction_horizon: Option<NodeId>,
    // ...
}
```

---

### 2. Atomic Causal Lineage Splitting

Compaction operates on the active causal branch (`CausalGraph::linear_path(active_leaf)`):
- **Fine-Grained Splitting**: Instead of discarding entire user rounds, the lineage evaluator calculates token density backwards from the newest node.
- **Tool Inseparability Invariant**: A cut point is strictly invalid if it isolates a `Role::Tool` result from its triggering `Role::Assistant` (tool call). The cut index automatically backtracks to the assistant call, guaranteeing that tool calls and their results are never split across the horizon boundary.

---

### 3. Programmatic Artifact Diffing & Intent Retention

- **Deterministic File Tracking**: Rather than asking the LLM to hallucinate which files were modified, the engine scans the folded nodes for `write_file`, `edit_text`, and `read_text` tool executions, synthesizing a 100% truthful `tracked_files` map.
- **User Intent Preservation**: User messages along the folded lineage are collected and preserved in a dedicated, bounded user-intent sequence (`COMPACT_USER_INTENT_TOKEN_BUDGET = 16_000`), ensuring that original specifications and constraints are never lost across multiple compaction epochs.
- **Structured Incremental Rollup**: When a prior compaction exists, the prompt instructs the model to merge the `<prior-summary>` with the new delta. The old summary is superseded; no recursive nesting of summaries occurs.

---

### 4. Zero Hard-Truncation Policy

- **Token Discipline at the Source**: Compaction generation passes `generation: { max_tokens: 4096 }` and uses explicit markdown section headers with concise bullet constraints.
- **Abolition of Slicers**: `truncate_summary_to_token_budget` is physically deleted. If an LLM output fails syntax or length validation, the system drops to the deterministic structured fallback generator, never slicing midway through tokens.

---

### 5. Four-Pass Compiler Layout & KV-Cache Stabilization

The compiler (`crates/muta-contracts/src/session_ir/compiler.rs`) orders the emitted request to maximize prefix reuse:

```text
Message 0: System Instructions (InstructionBundle)              [STATIC CACHE PREFIX]
Message 1: Workspace WorldState (Git status, active role)       [STATIC CACHE PREFIX]
Message 2: Compaction Checkpoint (Objective, Files, Milestones) [HORIZON CACHE PREFIX]
Message 3..K: Retained Historical User Prompts (Intent Chain)    [HORIZON CACHE PREFIX]
Message K+1..N: Volatile Retained Tail Dialogue                 [VOLATILE SUFFIX]
```

Pass 3 tags the boundary between Message $K$ and Message $K+1$ as the cache anchor, emitting a stable SHA256 fingerprint for upstream inference routing.

---

### 6. Eradication of Legacy Artifacts

1. **Delete**: `muta-persistence/src/session/ir_bridge.rs` and its deprecated conversions.
2. **Delete**: `muta-contracts/src/transcript.rs` and `ProjectionDirective`.
3. **Delete**: Transitional `SessionTree` and `SessionEntry` structures in `muta-contracts/src/session_tree.rs`.
4. **Refactor**: Rewrite `crates/muta-agent/src/compaction/` to export stateless transformations on `SessionIR`.
5. **Rewire**: `orchestration.rs` and `/compact` invoke the SessionIR compilation pipeline directly, reading zero data from `session.model_window()`.

---

## Invariants & Behavioral Boundaries

- **`[INV-COMPACT-01]` Single Source of Truth**: All compaction operations must operate directly upon `SessionIR` (`CausalGraph` and `SessionState`). No secondary transcript representations or projection directives may be created.
- **`[INV-COMPACT-02]` Atomic Tool Boundary**: A compaction horizon must never separate a tool call message from its corresponding tool result message. Cutting across an open tool execution is prohibited.
- **`[INV-COMPACT-03]` Prefix Stability Invariant**: Compaction must not alter the text or order of static system prompts and workspace baselines prior to the compaction checkpoint.
- **`[INV-COMPACT-04]` Ban on Destructive Token Slicing**: Post-generation truncation of summary text by tokenizers or character bounds is strictly forbidden. Summaries must remain well-formed Markdown structures.
- **`[INV-COMPACT-05]` Programmatic File Truth**: The `tracked_files` manifest in a compaction node must be computed directly from tool execution payloads, not generated via LLM inference.
- **`[INV-COMPACT-06]` Self-Healing Horizon**: If an LLM call for compaction fails with `ContextOverflow`, the splitter must shed the oldest 20% of candidate nodes and retry in-flight without aborting the session.

---

## Positive Consequences

- **100% KV-Cache Prefix Preservation**: Long sessions retain a stable prefill cache, reducing ongoing token costs by $70\text{--}85\%$ and cutting TTFT from ~8s to <800ms.
- **Elimination of Multi-Turn Amnesia**: User prompts and file artifacts survive infinitely across successive compaction rounds.
- **Clean Architecture**: Removes ~3,200 lines of transitional glue code, bridge mappers, and dual-track synchronization logic across `muta-contracts` and `muta-persistence`.
- **Sub-linear Compiler Complexity**: Context projection complexity drops from $O(N)$ (scanning entire session history) to $O(\text{tail})$, bounded by `compaction_horizon`.

---

## Negative Consequences & Trade-offs

- **Storage Layout Migration**: Existing SQLite databases with legacy transcripts must be migrated via a one-time migration that converts legacy transcript checkpoints into canonical `NodePayload::Compaction` nodes.
- **Increased Compaction Turn Complexity**: The compaction execution now performs artifact diffing and intent extraction, adding ~15ms of CPU preprocessing before the LLM summarization call.

---

## Rejected Alternatives & Negative Knowledge

### Retaining Transcript Projection Directives
- **Why considered**: Avoided breaking legacy session JSON files.
- **Why rejected**: Perpetuated the dual-track maintenance tax. Every new agent feature required dual implementation in both `Transcript` and `SessionIR`.

### Full User Message Retention Without Token Budgets (Naive Codex Replication)
- **Why considered**: Completely guarantees that no user prompt is ever modified.
- **Why rejected**: In human-agent sessions lasting 100+ rounds, raw user prompts alone can exceed 60,000 tokens, crowding out workspace code context. We enforce a 16,000 token budget for the intent chain.

### Destructive AST In-Memory Slicing (`truncate_summary_to_token_budget`)
- **Why considered**: Simple safety valve against rogue models ignoring output token limits.
- **Why rejected**: Slicing across token boundaries produced broken UTF-8 fragments, unclosed code fences, and truncated critical instructions, leading to severe downstream syntax and logic hallucinations.

---

## Links

- [ADR-0241: Session IR Causal Graph and Compiler Pipeline](0241-session-ir-causal-graph-and-compiler-pipeline.md)
- [ADR-0249: Session IR Native Execution Runtime and Durable Suspension](0249-session-ir-native-execution-runtime-and-durable-suspension.md)
- [ADR-0251: Causal DAG Branching Timelines and Unified Aside Multiverse](0251-causal-dag-branching-timelines-and-unified-aside-multiverse.md)
- [ADR-0254: Epistemic Observation Lifecycle, Ingestion Folding, and Near-Tail Invalidation](0254-epistemic-observation-lifecycle-ingestion-folding-and-near-tail-invalidation.md)
