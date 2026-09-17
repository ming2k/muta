# 0251. Causal DAG Branching Timelines, Unified Multiverse Session Routing, and Retirement of Ad-Hoc Aside Channels

- **Status:** Accepted
- **Date:** 2026-10-04
- **Scope:** `core/contracts`, `storage/persistence`, `runtime/session`, `terminal/mutx`
- **Deciders:** Muta Architecture Team
- **Builds on:** [ADR-0205](0205-unified-tui-surface-architecture-and-spatial-modality-taxonomy.md) (stage-scene-overlay architecture), [ADR-0238](0238-footer-chrome-advertises-only-bindings-that-fire.md) (truthful footer chrome), [ADR-0241](0241-session-ir-causal-graph-and-compiler-pipeline.md) (canonical session IR and causal graph), [ADR-0249](0249-session-ir-native-execution-runtime-and-durable-suspension.md) (session IR native execution), [ADR-0250](0250-role-anchored-session-isolation-and-self-excluding-switcher.md) (role-anchored session isolation)
- **Supersedes:** [ADR-0017](0017-side-conversations.md) (side conversations as detached views), [ADR-0103](0103-btw-background-asides.md) (background asides with ad-hoc `SideRegistry` and custom protocols)

---

## Context and Problem Statement

Muta originally introduced `/btw` in ADR-0017 as a modal side-conversation view that tore down the agent on exit. ADR-0103 subsequently converted asides into background turn-level conversations, but anchored them to an ad-hoc auxiliary structure: an in-memory `SideRegistry`, a secondary `ProxyProvider` agent wrapper, an isolated `side_messages` frontend buffer, and a separate set of six dedicated protocol events (`SideViewOpened`, `SideViewClosed`, `ParentStatus`, `QueryBtwList`, `FocusSide`, `CloseSide`).

When Muta executed the radical clean breaks for **Stage-Scene-Overlay** (ADR-0205) and **Canonical Session IR** (ADR-0241 / ADR-0249), the aside subsystem was never structurally modernized. This left four major architectural failures:

1. **The Dual-Buffer and Side-Channel Anti-Pattern**:
   The TUI maintained an explicit `app.in_side_view: bool` sideband and a dedicated `app.side_messages` buffer. Switching between the primary conversation and an aside bypassed the unified surface router and required custom message reassembly logic, violating ADR-0205's single-source-of-truth router contract.
2. **Causal Graph Fracture & Data Loss on Fork**:
   When `/btw` forked an aside, it invoked legacy `fork_to_side` which only duplicated legacy JSON/table rows. It completely failed to fork the canonical `SessionIR` in SQLite (`sessions_v2`, `session_policies`, `causal_nodes`). Furthermore, fallback conversions in `ir_bridge.rs` discarded rich `Message` payloads (tool calls, tool results, reasoning blocks), reducing inherited history to flat string content.
3. **Premature Destructive Discard on Detach**:
   Under ADR-0103 §4, stepping out of an un-sent aside triggered an unconditional delete of both its registry entry and its underlying session files. A user who opened an aside to inspect context and momentarily returned to the main session lost their aside completely.
4. **Suppression of Autonomous AI Titling**:
   Forking cloned parent session titles without clearing them, causing the autonomous background titler ([ADR-0022](0022-session-level-ai-title.md)) to see `has_title == true` and permanently refuse to generate titles for asides. Display titles fell back to crude character-truncated raw prompt lines.

We require a definitive, uncompromised architecture that models an Aside truthfully for what it is: **an alternative timeline cursor on the Session's immutable Causal DAG**, eliminating all ad-hoc registries and side channels.

---

## Decision Drivers

- **First-Principles Correctness**: In dialogue cognition, an aside is an exploratory branch off a specific turn. It shares the same workspace rules, role manifest, and root causal history, but advances its own branch cursor (`head_node`).
- **Mathematical Immutability**: All turns belong to a single DAG (`CausalGraph`). Lineage projection from any timeline cursor to root is a pure, idempotent traversal.
- **Zero Redundancy**: Ancestral nodes are stored and cached once in SQLite and in-memory IR, never copied into duplicate session blobs.
- **Frictionless UI Ergonomics**: Navigating between mainline and asides must be seamless via the unified Quick Switcher (`Ctrl+L` / `Ctrl+P`) and timeline breadcrumbs, with zero premature deletion.
- **Protocol Purity**: Eliminate custom side-channel wire messages in favor of uniform, session-native timeline operations.

---

## Decision Outcome

Execute an architectural clean break across `muta-contracts`, `muta-persistence`, `muta-runtime`, and `mutx`:

$$\text{Session} = \text{1 Immutable CausalGraph} + \text{1 SessionPolicy} + \text{N Named Timeline Cursors}$$

### 1. The Causal Timeline Model (`TimelineCursor`)

In `muta-contracts::session_ir`, we formalize timelines as first-class cursor entities in `SessionState`:

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineCursor {
    /// Unique timeline identifier (e.g. "main", "aside/3f8a...")
    pub id: String,
    /// Human or AI-generated title for the timeline
    pub name: String,
    /// Semantic timeline classification
    pub kind: TimelineKind,
    /// Active leaf causal node pointing to the head of this timeline
    pub head_node: Option<String>,
    /// Turn/node ID where this timeline branched off
    pub forked_from_node: Option<String>,
    pub created_at_s: u64,
    pub updated_at_s: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TimelineKind {
    Main,
    Aside,
    Speculation,
}
```

- Every session begins with a default `"main"` timeline pointing to the root node.
- Appending a dialogue node updates both `SessionState::active_leaf` and the active timeline's `head_node`.
- Forking via `/btw` executes `session_ir.create_timeline(name, TimelineKind::Aside, from_node)`.

### 2. Composite Causal Primary Keys & Lossless Migration 21

In `muta-persistence`, `causal_nodes` schema is upgraded from `id TEXT PRIMARY KEY` to `PRIMARY KEY (session_id, id)` (Migration 21). This permits shared or forked branch nodes to coexist across session and branch boundaries without SQLite primary-key conflicts.

`ir_bridge.rs` is made strictly lossless: `entry.to_message()` reassembles the complete `Message` structure (tool calls, tool call IDs, reasoning content, images, provider metadata), guaranteeing 100% fidelity during any projection.

### 3. Non-Destructive Lifecycle and Autonomous AI Titling

- **Non-Destructive Detach**: Detaching from an aside never destroys or deletes it. Pristine asides remain accessible in the registry and session timeline graph until explicitly closed by the user with `CloseSide` / `D`.
- **Clean Title Inheritance**: When an aside is spawned, `side.title` is initialized to `None`.
- **Asynchronous AI Titler Binding**: The aside agent is bound to `title_established`, triggering `agent.spawn_session_titler(...)` on the first user turn. Upon completion, the new title is persisted and broadcast to connected frontends in real time.

### 4. Registry-Driven Scene Header & Quick Switcher Integration

In `mutx`:
- **INV-3 Enforcement**: `ViewHints` carries `back_key: Option<Key>` dynamically resolved from `app.key_overrides.effective_binding(CommandId::CancelOrBack)`. Hardcoded `Key::ESC` and string literals are eradicated.
- **Truthful Timeline Breadcrumbs**:
  Breadcrumbs project timeline lineage: `SESSION [role] › ⑂ Aside: <AI Title> (from Turn N)`.
- **Top-Priority Switcher Targets**:
  `Quick Switcher` (`Ctrl+L` / `Ctrl+P`) surfaces all active timelines at the top of the search catalog, enabling instantaneous two-stroke switching between concurrent timelines.

---

## Invariants & Behavioral Boundaries

- **`[INV-TIMELINE-01]` Immutable Causal Lineage**: A timeline branch is strictly a DAG path projection (`head_node` $\to$ `parent_id` $\to \dots \to$ root). Nodes before the branch point are immutable and shared by identity.
- **`[INV-TIMELINE-02]` No Premature Destruction**: Detaching or navigating away from an aside must never delete session files or drop registry entries without explicit user confirmation (`CommandId::AsideClose` / `D`).
- **`[INV-TIMELINE-03]` Dynamic Keycap Truth**: Header breadcrumbs and affordance legends must never hardcode chords. All display keycaps must resolve from `GlobalOverrides` / `COMMAND_REGISTRY`.
- **`[INV-TIMELINE-04]` Autonomous Titler Parity**: Aside conversations must receive autonomous AI titling on their first user turn, with real-time broadcast to connected clients upon convergence.
- **`[INV-TIMELINE-05]` Zero Lossless Degradation**: Projecting between `TranscriptEntry` and `SessionIR` must never drop `tool_calls`, `tool_call_id`, `reasoning_content`, or structured attachments.

---

## Consequences

### Positive
- Eliminates the fractured dual-buffer architecture in the TUI (`app.in_side_view`, `app.side_messages`).
- Guarantees 100% causal node and tool execution fidelity across all aside forks.
- Unifies mainline conversations and asides under a single mathematical DAG model.
- Restores user muscle memory: switching back and forth between conversations and asides never causes state loss.

### Negative
- Requires Migration 21 to upgrade SQLite tables to composite primary keys.
- Requires slight adjustments to test harnesses verifying legacy aside disconnect behaviors.
