# 0262. Demand-Paged Epistemic Memory and Unified Inspect Channel

- **Status:** Proposed
- **Date:** 2026-10-21
- **Scope:** `core/contracts`, `core/runtime`, `muta-agent`, `muta-contracts`, `muta-persistence`
- **Deciders:** Muta Architecture Team
- **Builds on:** [ADR-0215](0215-tool-surface-consolidation.md) (Tool surface consolidation), [ADR-0241](0241-session-ir-causal-graph-and-compiler-pipeline.md) (Session IR causal graph), [ADR-0254](0254-epistemic-observation-lifecycle-ingestion-folding-and-near-tail-invalidation.md) (Epistemic observation lifecycle), [ADR-0255](0255-session-ir-native-causal-compaction-and-legacy-dual-track-eradication.md) (Session IR native causal compaction), [ADR-0261](0261-purge-of-nanny-prompting-and-attention-frugal-context-architecture.md) (Attention-frugal context architecture)
- **Extends:** [ADR-0187](0187-lock-free-event-ledger-and-wal-read-boundary.md) (Lock-free persistence reader)

---

## Context and Problem Statement

As autonomous agents engage in complex, multi-turn software development and system administration tasks (spanning 20–60+ turns and dozens of tool executions), active context pressure is aggressively mitigated by the three-tier relief ladder:
1. **Ingestion Folding & Pruning** ([ADR-0254](0254-epistemic-observation-lifecycle-ingestion-folding-and-near-tail-invalidation.md)): Clears verbose, superseded tool outputs (e.g. build logs, invalidated file reads, large search outputs) into placeholders.
2. **Causal Horizon Compaction** ([ADR-0255](0255-session-ir-native-causal-compaction-and-legacy-dual-track-eradication.md)): Folds historical dialogue turns and intermediate causal nodes behind an advancing `compaction_horizon`.
3. **Subagent Delegation** ([ADR-0186](0186-single-transcript-projection-directives-persistence.md)): Offloads deep exploratory and debugging sub-tasks into dedicated child sessions, returning only a consolidated summary to the parent transcript.

While these mechanisms successfully bound prompt size, protect KV-cache prefixes, and prevent token overflow, they introduce a critical architectural blind spot: **Epistemic Amnesia (Cognitive Black Hole)**.

Once an artifact exits the active context window:
* A pruned tool output is replaced with a static text placeholder (`[cleared tool result: ...]`). If the model realizes 10 turns later that it needs a specific line range from an earlier file read or the specific stack trace from an earlier build failure, the original text is inaccessible through the model surface.
* A subagent transcript is preserved in SQLite as an isolated session, but the parent model only retains the summarized return message. If the subagent failed, was interrupted by the operator (`Ctrl+C`), or omitted an essential nuance, the master agent cannot inspect the child's raw execution trajectory.
* A folded causal subgraph is compressed into a compaction checkpoint node. Granular assertions, rejected hypotheses, or configuration specifics discussed early in the session cannot be re-examined.

Lacking an addressable read mechanism, models exhibit **Information Hoarding**: fighting against compaction, pleading with the operator to avoid subagents, and re-reading large files repeatedly.

---

## Decision Drivers

- **Zero Legacy Burden (Clean Break)**: Reject hybrid dual-track schemes relying on deprecated transcript sequences (`seq:from-to`). Implement off-stream access natively on Session IR causal graphs and CAS blob stores.
- **Demand-Paged Epistemic Virtual Memory**: Treat active context as L1 Cache and durable storage as backing store. Allow models to pull off-stream content on demand with deterministic, bounded token consumption.
- **Strict Tool Orthogonality ([ADR-0215](0215-tool-surface-consolidation.md))**: Consolidate off-stream memory access into a single unified `inspect` tool. Strictly exclude operating-system background process/job logs (`job:`), which belong exclusively to the `process` tool.
- **Zero Wire Protocol / Schema Churn**: Avoid mutating wire types such as `SubagentRef` that trigger downstream TypeScript code generation churn. Dynamically derive subagent states from child session termination nodes.
- **Self-Describing Handles**: Automatically embed addressable causal URIs directly inside prune placeholders, compaction checkpoints, and subagent completion headers, enabling $O(1)$ discovery.
- **Bounded Attention Economics**: Enforce physical token budgets (default 4,096 tokens, hard cap 8,192 tokens), regex-prefiltered grep queries, and opaque continuation cursors.

---

## Invariants & Behavioral Boundaries

- **`[INV-OFFSTREAM-01]` Epistemic Read-Only Integrity**: `inspect` is strictly a passive observer. It never performs causal grafting, history rewriting, or branch mutation. The returned content enters the active session strictly as a standard `ToolResult`, subject to normal downstream lifecycle policies.
- **`[INV-OFFSTREAM-02]` Domain Orthogonality**: `inspect` is strictly reserved for cognitive and session history artifacts (`sub:`, `call:`, `fold:`). OS process/job management and daemon logs remain exclusively with `process`.
- **`[INV-OFFSTREAM-03]` Session IR Native Topology**: All off-stream addressing references `CausalNodeId` and `SessionId`. No references to legacy flat transcript sequence integers are permitted.
- **`[INV-OFFSTREAM-04]` Root Master Privilege**: `inspect` is registered exclusively in the master agent toolset. Subagent execution profiles (`explore`, `debug`, `skill`) are strictly prohibited from inheriting `inspect` to prevent recursive transcript hoarding.
- **`[INV-OFFSTREAM-05]` Self-Describing Inline Handles**: Any subsystem that clears or folds context (pruning in `pressure.rs`, compaction in `causal_compactor.rs`, settlement in `subagent_tool.rs`) MUST embed an explicit `inspect` handle in the generated placeholder.
- **`[INV-OFFSTREAM-06]` Lock-Free Non-Blocking Execution ([ADR-0187](0187-lock-free-event-ledger-and-wal-read-boundary.md))**: Offstream sources access persistence via read-only reader handles. No session locks or writer mutexes may be acquired during `inspect` execution.

---

## Technical Architecture

### 1. Causal Resource URI Scheme

Three canonical schemes are established:

| Handle Scheme | Entity | Underlying Storage | Example |
| :--- | :--- | :--- | :--- |
| `sub:<session_id>` | Child Subagent Session | Session IR causal history (`history.linear_path`) | `sub:ses_019a2b3c` |
| `call:<tool_call_id>` | Pruned Tool Result | CAS BlobStore (`content_blob`) / Archived Causal Node | `call:call_99f01ab` |
| `fold:<compaction_node_id>` | Folded Causal Subgraph | Historical nodes behind `compaction_horizon` | `fold:node_77e12c` |

### 2. Contracts & Abstraction (`muta-contracts::offstream`)

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OffstreamStatus {
    Ready,
    Archived,
    Pruned,
    Compacted,
    Failed,
    Interrupted,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OffstreamEntry {
    pub handle: String,
    pub label: String,
    pub status: OffstreamStatus,
    pub size_tokens: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PagedOffstreamContent {
    pub text: String,
    pub next_cursor: Option<String>,
    pub total_lines: usize,
}

#[async_trait]
pub trait OffstreamSource: Send + Sync {
    fn scheme(&self) -> &'static str;
    async fn enumerate(&self, session_id: &str) -> Result<Vec<OffstreamEntry>, String>;
    async fn read(
        &self,
        key: &str,
        cursor: Option<&str>,
        query: Option<&str>,
        budget_tokens: usize,
    ) -> Result<PagedOffstreamContent, String>;
}
```

### 3. Unified Inspect Tool (`muta-agent::tools::inspect`)

The model interacts with off-stream memory via a single tool with progressive disclosure:
- **`action: "list"`**: Enumerates all off-stream artifacts associated with the current session lineage into a compact Markdown table.
- **`action: "read"`**: Reads the specified `handle`. Supports:
  - `query`: Optional regex/substring filter applied prior to pagination, returning matching blocks with turn/line anchors.
  - `cursor`: Opaque token-pagination cursor returned in earlier truncated responses.
  - `budget_tokens`: Clamped between 512 and 8,192 tokens (default: 4,096 tokens).

### 4. Self-Describing Inline Integration

Placeholders generated across the codebase are upgraded to explicitly disclose their handle:
1. **Pruning** (`muta-contracts/src/pressure.rs`):
   ```text
   [cleared tool result: read config.rs (42 lines, 350 tokens) — inspect with handle "call:call_xyz"]
   ```
2. **Causal Compaction** (`muta-agent/src/compaction/causal_compactor.rs`):
   ```text
   [Context compaction checkpoint: 14 nodes folded — inspect with handle "fold:node_abc"]
   ```
3. **Subagent Settlement** (`muta-agent/src/subagent_tool.rs`):
   ```text
   [Subagent execution settled (status: ready). Full transcript inspect handle: "sub:ses_123"]
   ```

---

## Alternatives Considered and Rejected

1. **Re-Injecting / Un-Compacting History into the Active Session**:
   * *Rejected*. Grafts old nodes into the active branch, busting KV prompt caching across the entire history and destabilizing current attention.
2. **Including OS Process and Daemon Logs (`job:<id>`) in `inspect`**:
   * *Rejected*. Violates ADR-0215 (Tool Surface Consolidation). `process` is the authoritative lifecycle owner of background commands and services. Adding `job:` creates tool confusion.
3. **Flat Transcript Sequence Addressing (`seq:<from>-<to>`)**:
   * *Rejected*. Breaks the clean-break mandate of ADR-0255. Flat sequence numbers are obsolete artifacts of deprecated transcript storage.
4. **Mutating `SubagentRef` Schema for Status**:
   * *Rejected*. Causes unnecessary churn on the TypeScript client code generator (`wire.gen.ts`). Child session termination nodes already hold authoritative execution status.

---

## Consequences

### Positive
- Models can fearlessly delegate to subagents and permit aggressive pruning/compaction, knowing all dropped context is recallable.
- Eliminates redundant tool re-executions (e.g. re-reading large files to recover previously discarded sections).
- Maintains strict 100% KV-cache prefix stability by keeping all retrieved history in standard tail tool results.

### Negative / Trade-offs
- Adds one new tool schema (`inspect`) to the master toolset (~250 tokens in tool definitions).
- Requires runtime integration with the persistence reader to resolve child sessions and CAS blobs.

---

## References

- [ADR-0187: Lock-Free Persistence Reader](0187-lock-free-event-ledger-and-wal-read-boundary.md)
- [ADR-0215: Tool Surface Consolidation](0215-tool-surface-consolidation.md)
- [ADR-0241: Session IR Causal Graph](0241-session-ir-causal-graph-and-compiler-pipeline.md)
- [ADR-0254: Epistemic Observation Lifecycle](0254-epistemic-observation-lifecycle-ingestion-folding-and-near-tail-invalidation.md)
- [ADR-0255: Session IR Native Causal Compaction](0255-session-ir-native-causal-compaction-and-legacy-dual-track-eradication.md)
- [ADR-0261: Attention-Frugal Context Architecture](0261-purge-of-nanny-prompting-and-attention-frugal-context-architecture.md)
