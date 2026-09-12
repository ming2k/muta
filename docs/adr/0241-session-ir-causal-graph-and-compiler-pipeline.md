# 0241. Session IR: Causal graph, unified persistence schema, and multi-pass compiler pipeline

- **Status:** Proposed
- **Date:** 2026-09-30
- **Scope:** `core/session`, `persistence/schema`, `providers/compiler`
- **Deciders:** Muta Architecture Team
- **Supersedes (data model & schema):** ADR-0040 (session state dual arrays), ADR-0186 (single-transcript model as a flat list), amends ADR-0187 (persistence v2 storage schema)

---

## Context and Problem Statement

A session in an AI coding agent is the fundamental boundary of state, memory, and cognitive action. Historically, muta's session model evolved through piecemeal tranches:
1. **The Dual-Representation Schism**: In-memory and on-disk representations are fractured between two competing paradigms: `SessionTree` (a DAG designed to serve TUI `/tree` branch visualization) and `Transcript` (an append-only sequence of `TranscriptEntry` designed by ADR-0186). Switching branches (`switch_tree_leaf`) violently purges and re-executes `rebuild_transcript_from_messages`, forcing an artificial sync between two coexisting truths.
2. **Terminal UI Leakage into Core Schema**: The database schema (`sessions`, `entries`) has been progressively corrupted by terminal presentation concerns. Fields such as `sessions.commands` (raw slash-command execution logs), `sessions.fork_kind CHECK ('aside')` (a terminal layout drawer mode), `sessions.last_user_prompt` and `sessions.msg_count` (denormalized UI card snippet cache), and `entries.hidden` / `display_content` (terminal rendering flags) degrade the storage layer into a TUI view-state dumping ground.
3. **Imperative Request Assembly**: Constructing provider wire payloads (Anthropic `/messages`, OpenAI chat/responses, Gemini `/v1beta`) is performed through ad-hoc imperative assembly. Protocol quirks (e.g. Gemini thought signatures, Anthropic thinking blocks, KV-cache breakpoints) bleed into core prompt generation rather than being isolated behind an optimizing compiler pipeline.

We require a radical, clean-break unification: establish a canonical in-memory **Session Intermediate Representation (Session IR)**, align physical persistence strictly 1:1 with this IR while purging all UI artifacts, and formalize request generation as an on-demand, multi-pass **Compiler Pipeline**.

---

## Decision Drivers

- **Long-term Architectural Purity**: The core domain model of an agent session must be headless, platform-agnostic, and completely independent of any presentation client (Terminal, Web, IDE, or Daemon).
- **Single Source of Truth**: Eliminate dual-representation reconciliation (`SessionTree` vs `Transcript`). One canonical data structure must represent dialogue history, branching, and checkpoints.
- **Forensic Execution Preservation**: Critical runtime control events—such as human interrupts (`Ctrl+C`), suspended approval gates (`NeedsApproval`), and asynchronous system notices (`SystemWake`)—must be preserved with causal exactness rather than stored as detached, floating JSON sidecars.
- **KV-Cache Prefix Determinism**: Compiling wire requests must provide mathematical prefix stability across turns, automatically anchoring provider prompt-caching breakpoints (`cache_control`) without manual prompt tinkering.
- **$O(\Delta)$ Incremental Durability**: Persisting mutations must remain bounded by the size of the turn delta, avoiding $O(N)$ tree scans or full-state re-serialization.

---

## Considered Options

- **Option 1: Evolutionary Patching (Keep Transcript + Tree dual model, add more bridge methods)**. Continue syncing `SessionTree` and `Transcript` via background projections; add helper triggers in SQLite.
- **Option 2: AST-Centric Document Tree (Virtual DOM style)**. Represent the session as a generic hierarchical syntax tree with arbitrary sub-node mutations and run a generic recursive tree-diffing engine on every commit.
- **Option 3 (Chosen): Canonical Session IR (Causal Graph + Working Registers + Policy) with Delta Persistence and Multi-Pass Request Compilation**. A clean-break architecture establishing an immutable causal graph, a structured register state machine, an explicit policy contract, and a multi-pass compiler targeting LLM wire protocols.

---

## Decision Outcome

Chosen option: **Option 3**. We execute an uncompromising clean break across `muta-contracts`, `muta-persistence`, `muta-runtime`, and `muta-providers`.

### 1. The Canonical Session IR

The in-memory session is formally modeled as a triad:

```rust
pub struct SessionIR {
    /// 1. History: Immutable Causal Fact Graph
    pub history: CausalGraph,

    /// 2. State: Mutable Working State & Cursor Registers
    pub state: SessionState,

    /// 3. Policy: Governing Rules, Capabilities, and Guardrails
    pub policy: SessionPolicy,
}
```

#### A. Causal Fact Graph (`history`)
- **Node Immutability**: Every entry (`NodeId`, `parent_id`, `seq`, `payload`) is immutable once inserted.
- **Node Kinds**:
  - `Dialogue`: User message, Assistant response (with tool calls and reasoning), Tool execution result.
  - `Compaction`: Summarization checkpoint replacing ancestral branches up to an anchor node.
  - `Termination`: User interrupt (`Interrupted { reason, at_ms, partial_output }`), error stop.
  - `SystemNotice`: External asynchronous event injection (e.g. background job completion, environment drift).
- **Topological Invariant**: Every node except root has an explicit `parent_id`. Branching and forks are ordinary graph topologies; linear conversation is a path projection.

#### B. Working State Registers (`state`)
- **Active Cursor**: `active_leaf: NodeId` represents the tip of the currently active branch.
- **Execution Status Machine**:
  ```rust
  pub enum ExecutionStatus {
      Idle,
      Running { current_turn: u64 },
      Suspended(SuspensionReason),
  }

  pub enum SuspensionReason {
      NeedsApproval { tool_call_id: String, action: String },
      NeedsInput { prompt: String },
      RetryPending { retry_token: String, attempts: u32 },
  }
  ```
- **Pending Event Mailbox**: `pending_notifications: Vec<SystemNotice>` queues unprocessed signals arriving during inactive or suspended windows.

#### C. Governing Policy (`policy`)
- **Rules**: Static system directives, role personas, and workspace governance (`AGENTS.md`).
- **Capabilities**: Whitelisted tools, registered MCP servers, active extensions.
- **Guardrails**: Command approval thresholds, read-only filesystem boundaries.
- **Budget**: Maximum context window, compaction triggers, token allocation limits.

---

### 2. Clean Persistence Schema (Purging Presentation Leakage)

Persistence becomes a pure, 1:1 serialization and hydration engine for `SessionIR`. All UI presentation artifacts are expelled from the core schema.

```
                    ┌──────────────────────────────────────────────┐
                    │                  SessionIR                   │
                    │       (History + State + Policy)             │
                    └──────────────────────┬───────────────────────┘
                                           │
                         Hydrate / Drain (1:1 Isomorphism)
                                           │
                                           ▼
┌──────────────────────────────────────────────────────────────────────────────────┐
│ Canonical SQLite Schema (Headless, Cross-Platform Domain Kernel)                  │
├──────────────────────────────────────────────────────────────────────────────────┤
│ 1. `sessions`                                                                    │
│    id TEXT PRIMARY KEY,                                                          │
│    parent_session_id TEXT REFERENCES sessions(id),                               │
│    active_node_id TEXT NOT NULL,                                                 │
│    status TEXT NOT NULL, -- 'idle' | 'running' | 'suspended'                     │
│    suspension_json TEXT, -- structured suspension payload when suspended         │
│    created_at_s INTEGER NOT NULL,                                                │
│    updated_at_s INTEGER NOT NULL                                                 │
│                                                                                  │
│ 2. `session_policies`                                                            │
│    session_id TEXT PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,        │
│    workspace_root TEXT,                                                          │
│    model_pin TEXT,                                                               │
│    rules_json TEXT NOT NULL,                                                     │
│    capabilities_json TEXT NOT NULL,                                              │
│    guardrails_json TEXT NOT NULL,                                                │
│    budget_json TEXT NOT NULL                                                     │
│                                                                                  │
│ 3. `causal_nodes` (Replaces legacy `entries`, `entry_memberships`, `sessions.tree`)│
│    id TEXT PRIMARY KEY,                                                          │
│    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,           │
│    parent_id TEXT REFERENCES causal_nodes(id),                                   │
│    seq INTEGER NOT NULL,                                                         │
│    kind TEXT NOT NULL CHECK (kind IN ('dialogue','compaction','termination','notice')),│
│    role TEXT CHECK (role IN ('user','assistant','tool','system')),                │
│    payload_json TEXT NOT NULL,                                                   │
│    created_at_ms INTEGER NOT NULL,                                               │
│    UNIQUE(session_id, seq)                                                       │
└──────────────────────────────────────────────────────────────────────────────────┘
```

#### Expelled Schema Artifacts (Negative Knowledge & Anti-Patterns)
- **DELETED** `sessions.commands`: TUI slash-command invocation logs belong to user activity auditing, not the conversation state.
- **DELETED** `sessions.fork_kind CHECK ('aside')`: Terminal split-pane layout modes must never define database column constraints.
- **DELETED** `sessions.last_user_prompt`, `sessions.msg_count`: UI list picker optimizations move to an ephemeral or client-side SQLite materialization view.
- **DELETED** `sessions.tree TEXT`: Storing a serialized JSON tree alongside relational entries is banned. The `causal_nodes` table *is* the tree.
- **DELETED** `entries.hidden`, `MessagePayload.display_content`: Presentation visibility and terminal markdown styling are pure client rendering decisions.
- **DELETED** `input_history`: Terminal readline input buffers move to client-specific storage (`~/.local/share/muta/terminal_history.db`).

---

### 3. Multi-Pass Request Compilation Pipeline

Converting `SessionIR` into an outbound LLM HTTP request is formalized as an on-demand compilation pipeline:

```text
[SessionIR: history, state, policy]
         │
         ▼  Pass 1: Active Branch Projection
[Linear Causal Slice (active_leaf ──> root / checkpoint)]
         │
         ▼  Pass 2: Context Compaction & Budget Allocation
[Budgeted Dialogue Frame]
         │
         ▼  Pass 3: Cache Boundary & Prefix Stabilization
[Canonical Prefix + Ephemeral Dynamic Suffix]
         │
         ├───────────────────────────────┬───────────────────────────────┐
         ▼                               ▼                               ▼
[Anthropic Lowering]            [OpenAI Lowering]               [Gemini Lowering]
- Ephemeral cache_control       - Tool trace validation         - Thought signature bonding
- Thinking block integrity      - Responses API schema          - FunctionCall pairing
```

- **Pass 1 (Active Branch Projection)**: Traverses backward from `state.active_leaf` following `parent_id` pointers until hitting a `Compaction` checkpoint or the root. Yields an ordered linear sequence of active turns.
- **Pass 2 (Context Compaction & Budget Allocation)**: Applies token limits defined in `policy.budget`. Large tool results on stale turns are folded or summarized.
- **Pass 3 (Cache Boundary & Prefix Stabilization)**: Partitions the request into a **Static Prefix** (System Rules + Workspace Policies + Settled Turns) and an **Ephemeral Suffix** (Latest Turn + Active Tools). Computes a deterministic cache fingerprint.
- **Pass 4 (Backend Lowering)**: Compiles the canonical frame into target wire formats:
  - *Anthropic*: Injects `cache_control` breakpoints, preserves thinking signatures.
  - *OpenAI*: Lowers into ChatCompletions / Responses API payloads with structured tool outputs.
  - *Gemini*: Pairs consecutive `functionCall` and `functionResponse` blocks and embeds required thought signatures.

---

### Invariants & Behavioral Boundaries

- **`[INV-SESSION-01]` Single Source of Truth**: The `SessionIR` struct is the exclusive in-memory authority for a session. No parallel tree, transcript, or shadow array may be maintained.
- **`[INV-SESSION-02]` Hydration Parity**: For any valid session, `Hydrate(Drain(SessionIR)) == SessionIR`. The database schema must be a lossless representation of the in-memory IR.
- **`[INV-SESSION-03]` Presentation Isolation**: No database table, domain struct, or compiler pass within the session kernel may reference terminal layout, formatting, or client-specific interaction conventions.
- **`[INV-SESSION-04]` Forensic Termination**: A user interrupt (`Ctrl+C` / cancel verb) MUST be committed as a terminal causal node or turn termination state, recording the exact interrupted tool call or partial output. An interrupt is never an ephemeral discard.
- **`[INV-SESSION-05]` $O(\Delta)$ Commit Bound**: Persisting a turn must only write nodes with `seq > last_committed_seq` and shallow-update the `sessions` row. Full graph traversals during write transactions are strictly forbidden.
- **`[INV-SESSION-06]` Pure Lowering**: Request compilation from `SessionIR` to provider payloads must be a pure, side-effect-free transformation. Modifying session state during prompt compilation is prohibited.

---

### Positive Consequences

1. **Elimination of Architectural Friction**: Eradicates the painful sync between `SessionTree` and `Transcript`. Branching, undoing, and resuming become trivial pointer operations on the causal graph.
2. **True Platform Independence**: The persistence engine and session runtime become 100% headless. muta can run as a daemon, a Web service, an IDE backend, or a TUI without a single schema alteration.
3. **Forensic Integrity**: Full preservation of interrupts, approval states, and asynchronous notifications ensures crash-resilient session resumption.
4. **Optimal Provider Caching**: First-class cache boundary analysis guarantees maximum KV-cache hit ratios across all supported model providers, slashing API costs and time-to-first-token.

---

### Negative Consequences & Trade-offs

- **Migration Complexity**: Rebuilding existing SQLite databases (`v14` -> `v15`) requires migrating legacy `entries`, `entry_memberships`, and `sessions.tree` into the unified `causal_nodes` table.
- **Transitional Codebase Churn**: Refactoring `muta-contracts`, `muta-persistence`, and `muta-runtime` requires updating thousands of call sites that previously expected a flat `transcript` array.
- **Mitigation**: Implement a verified, one-way schema migration script with full backup guarantees, and provide compatibility bridge views (`IR.resolve_linear_transcript()`) during the transitional phase.

---

### Rejected Alternatives & Negative Knowledge

#### Rejected: Retaining the Dual Model with Auto-Sync Triggers
- **Why considered**: Avoids rewriting `crates/muta-persistence/src/session/history.rs`.
- **Why rejected**: Perpetuates dual-representation drift. SQLite triggers cannot maintain consistency with complex in-memory compaction pointers. Fundamentally violates Clean-Break architecture.

#### Rejected: Generic DOM/AST Tree with Recursive Diffing
- **Why considered**: Follows classic Virtual DOM / UI tree architectures.
- **Why rejected**: Conversation histories are 99% append-only. Generic tree diffing introduces $O(N \log N)$ compute overhead and lock contention on every dialogue turn, directly violating latency SLAs.

#### Rejected: Embedding TUI Presentation Views as Real-Time Database Triggers
- **Why considered**: Keeps session list queries fast for the terminal picker.
- **Why rejected**: Violates `[INV-SESSION-03]`. Client display performance must be solved via client-side caching or dedicated read-only projection layers (CQRS), never by contaminating the core write model.

---

## Links

- Supersedes: [ADR-0040 (Session state & context projection)](0040-session-state-and-context-projection.md)
- Amends: [ADR-0186 (Single transcript persistence)](0186-single-transcript-projection-directives-persistence.md)
- Amends: [ADR-0187 (Persistence v2 incremental append)](0187-persistence-v2-incremental-append-and-blob-reference-ledger.md)
- Related: [ADR-0217 (Cache prefix stability)](0217-context-compaction-resilience.md), [ADR-0218 (Request projection archive)](0218-durable-request-projection-archive.md)
- Architecture Blueprint: [`docs/architecture/session-ir.md`](../architecture/session-ir.md)
