# Session IR Architecture

The Session Intermediate Representation (Session IR) is the canonical, in-memory domain model and single source of truth for an agent conversation. It provides a platform-agnostic, headless kernel governing memory, execution status, and policy constraints, while decoupling core cognitive mechanics from physical storage layouts and downstream LLM wire protocols.

```text
                                 ┌──────────────────────────────────────────────┐
                                 │                  SessionIR                   │
                                 ├──────────────────────────────────────────────┤
                                 │ 1. history: CausalGraph (Append-only Facts)  │
                                 │ 2. state:   SessionState (Registers & Cursor)│
                                 │ 3. policy:  SessionPolicy (Decision Bounds)  │
                                 └──────────────┬───────────────────────────────┘
                                                │
                 ┌──────────────────────────────┴──────────────────────────────┐
                 │                                                             │
                 ▼                                                             ▼
  [ Persistence Engine (Drain/Hydrate) ]                    [ Multi-Pass Compiler Pipeline ]
  - 1:1 Domain Isomorphism                                  - Pass 1: Active Branch Projection
  - O(Δ) Incremental Sequence Commits                       - Pass 2: Context Projection & Compaction
  - Causal Integrity (No Presentation Leaks)                - Pass 3: Cache Boundary & Prefix Stability
  - Forensic Scene Recovery (Interrupts/Approval)           - Pass 4: Target Backend Lowering (Anthropic/OAI/Gemini)
                 │                                                             │
                 ▼                                                             ▼
      [ Canonical SQLite DB ]                                     [ LLM Provider Wire APIs ]
```

---

## 1. The Core Triad

The Session IR is composed of three orthogonal axes:

```rust
pub struct SessionIR {
    pub history: CausalGraph,
    pub state: SessionState,
    pub policy: SessionPolicy,
}
```

### A. History (`CausalGraph`)
`CausalGraph` represents the immutable record of historical facts. Every turn, tool invocation, compaction event, and external signal is captured as a node in an acyclic directed graph:

- **Node Immutability**: Once appended, a node's content, timestamp, sequence number, and provenance can never be modified.
- **Topological Causality**: Every node (except the root) points explicitly to its `parent_id`. Branching, forks, and subagent side-tracks are first-class topological branches rather than copied arrays.
- **Node Kinds**:
  - `Dialogue`: Real conversation exchanges (User, Assistant with tool calls, Tool results).
  - `Compaction`: An explicit checkpoint summarizing an ancestral path up to an anchor node.
  - `Termination`: Records where an execution turn was cut short (e.g. human interrupt, network fault, runtime timeout).
  - `SystemNotice`: Durable background events (e.g. background job completion, workspace file change notifications).

### B. State (`SessionState`)
`SessionState` represents the mutable working memory and execution cursor:

- **Active Cursor (`active_leaf: NodeId`)**: Identifies the tip of the branch currently being traversed or appended to. Switching branches is an $O(1)$ register update pointing to a different leaf.
- **Execution Status Machine**:
  ```rust
  pub enum ExecutionStatus {
      Idle,
      Running { turn: u64, started_at_ms: u64 },
      Suspended(SuspensionReason),
  }

  pub enum SuspensionReason {
      NeedsApproval { tool_call_id: String, action: String },
      NeedsInput { prompt: String },
      RetryPending { retry_token: String, attempts: u32 },
  }
  ```
- **Pending Event Mailbox (`pending_notifications: Vec<SystemNotice>`)**: Holds asynchronous signals arriving while the session is suspended or offline, guaranteeing zero event loss upon wake.

### C. Policy (`SessionPolicy`)
`SessionPolicy` formalizes the behavioral constraints and capabilities granted to the agent:

- **Rules (`RuleSet`)**: Static system personas, cognitive style, and workspace governance (`AGENTS.md`).
- **Capabilities (`CapabilityPolicy`)**: Whitelisted tools, enabled MCP servers, and available skills.
- **Guardrails (`GuardrailPolicy`)**: Command safety classifications, human approval thresholds, and read-only filesystem boundaries.
- **Budget (`BudgetPolicy`)**: Maximum token limits, sliding window thresholds, and compaction triggers.

---

## 2. Forensic Scene Preservation

A primary design guarantee of the Session IR is **complete, forensic scene restoration**. A session recovered from cold storage must reproduce the exact state of execution without ambiguity:

1. **Human Interrupts (`Ctrl+C` / Cancel Verbs)**:
   - An interrupt is an immutable causal fact. It is anchored to the exact node being generated or tool being executed.
   - The resulting `Termination` node stores the partial assistant output, the interrupted tool call ID, and the elapsed time.
   - On resume, the agent knows precisely what was truncated and why, preventing hallucinated completions.
2. **Approval & Input Gates (`NeedsApproval` / `NeedsInput`)**:
   - If an agent prompts the user for approval on a destructive action (`rm -rf`) and the host process reboots, `SessionState.status` restores to `Suspended(NeedsApproval)`.
   - The session refuses to dispatch further model calls until the approval gate is resolved.
3. **Asynchronous Signals (`SystemWake`)**:
   - Notifications from background tasks (ADR-0234) are committed directly to the graph or held in the pending mailbox, guaranteeing deterministic waking.

---

## 3. Persistence & Clean Storage Boundary

Persistence is an isomorphic projection of `SessionIR`. Storage exists solely to drain in-memory mutations to disk and hydrate them back on demand:

$$\text{Database} \xrightarrow{\quad \text{Hydrate} \quad} \text{SessionIR} \xrightarrow{\quad \text{Drain} \quad} \text{Database}$$

### Physical SQLite Schema
The relational schema reflects the triad directly:
- **`sessions`**: Stores the session cursor, lifecycle metadata, and `ExecutionStatus`.
- **`session_policies`**: Stores serialized governance, capabilities, guardrails, and budget constraints.
- **`causal_nodes`**: Stores all graph nodes indexed by `(session_id, seq)`. `parent_id` foreign keys preserve graph topology.

### Purged Presentation Anti-Patterns
All terminal UI artifacts are purged from the domain persistence model:
- **No Slash-Command Dumps**: TUI commands (`/search`, `/tree`) belong to client input streams, not conversation history.
- **No Presentation Modes in Constraints**: Layout primitives (`aside` split views) never appear in database `CHECK` constraints.
- **No Denormalized Card Caches**: Snippets like `last_user_prompt` and `msg_count` are derived by queries or client views, never stored as primary row fields.
- **No Dual Representations**: The separate JSON `sessions.tree` column is abolished; `causal_nodes` is the single source of truth for the DAG.
- **No Client Input Buffers**: Readline `input_history` is relegated to client-side configuration databases.

### Incremental Commits ($O(\Delta)$)
Turn commits append only nodes with `seq > last_committed_seq` and perform a shallow update on `sessions`. Write operations remain sub-millisecond regardless of session history length.

---

## 4. Multi-Pass Compiler Pipeline

Generating an outbound LLM request is structured as a multi-stage optimizing compilation pipeline:

```text
[SessionIR (history, state, policy)]
         │
         ▼  Pass 1: Active Branch Projection
[Linear Causal Path: active_leaf ──> root / checkpoint]
         │
         ▼  Pass 2: Context Projection & Compaction
[Budget-Bounded Dialogue Sequence]
         │
         ▼  Pass 3: Cache Boundary Analysis & Prefix Stabilization
[Static Prefix Block | Ephemeral Tail Block]
         │
         ├───> [Anthropic Backend Lowering]  (cache_control, thinking blocks)
         ├───> [OpenAI Backend Lowering]     (tool schemas, responses payload)
         └───> [Gemini Backend Lowering]     (thought signatures, function call pairing)
```

1. **Pass 1 (Active Branch Projection)**: Resolves the active linear lineage by following `parent_id` pointers backwards from `state.active_leaf` until terminating at a `Compaction` node or the graph root.
2. **Pass 2 (Context Projection & Compaction)**: Evaluates `policy.budget` against cumulative token weight. Historical tool results exceeding budget are replaced with structured reference placeholders.
3. **Pass 3 (Cache Boundary Analysis)**: Partitions the sequence into:
   - **Static Prefix**: System instructions (`policy.rules`), workspace constraints, and stabilized conversation turns. Computes a deterministic cache fingerprint.
   - **Ephemeral Suffix**: Dynamic user input, scratchpads, and active tool calls.
4. **Pass 4 (Target Backend Lowering)**: A pure code-generation pass transforming the canonical frame into provider-specific schemas:
   - *Anthropic Messages*: Inserts `ephemeral` `cache_control` markers at the static prefix boundary; preserves thinking block signatures.
   - *OpenAI*: Formats tool definitions and maps reasoning tokens to provider fields.
   - *Gemini*: Emits strictly paired `functionCall` / `functionResponse` turns and embeds opaque thought signatures.

---

## 5. Architectural Lineage

- **Founding ADR**: [ADR-0241: Session IR, Causal Graph, and Multi-Pass Compiler Pipeline](../adr/0241-session-ir-causal-graph-and-compiler-pipeline.md)
- **Supersedes**: [ADR-0040](../adr/0040-session-state-and-context-projection.md) (dual array storage)
- **Amends**: [ADR-0186](../adr/0186-single-transcript-projection-directives-persistence.md) (single-transcript model), [ADR-0187](../adr/0187-persistence-v2-incremental-append-and-blob-reference-ledger.md) (persistence v2 schema)
- **Related Principles**: [ADR-0217](../adr/0217-context-compaction-resilience.md) (prefix stability), [ADR-0236](../adr/0236-durable-turn-commits-and-recoverable-projections.md) (durable turn commits)
