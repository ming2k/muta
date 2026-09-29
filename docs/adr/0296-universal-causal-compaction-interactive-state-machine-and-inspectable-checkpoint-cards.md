---
id: ADR-0296
title: "Universal Causal Compaction, Interactive State Machine, and Inspectable Checkpoint Cards"
status: accepted
date: 2026-10-26
scope: contracts/events, runtime/commands, agent/compaction, mutx/tui
superseded_by: null
negative_knowledge: true
---

# 0296. Universal Causal Compaction, Interactive State Machine, and Inspectable Checkpoint Cards

- **Status:** Accepted
- **Date:** 2026-10-26
- **Scope:** `muta-contracts` (events, projection payloads), `muta-agent` (compaction cut point, summarization), `muta-runtime` (slash commands, state dispatch), `mutx` (transcript cards, keyboard interaction)
- **Deciders:** Muta Architecture Team
- **Builds on:** [ADR-0255](0255-session-ir-native-causal-compaction-and-legacy-dual-track-eradication.md) (Session IR native causal compaction), [ADR-0278](0278-compaction-as-a-view-commit.md) (Compaction as a view commit), [ADR-0283](0283-anti-thrashing-context-scheduling-and-generative-horizon-compaction.md) (Anti-thrashing context scheduling and generative horizon compaction)
- **Amends:** [ADR-0110](0110-commands-do-not-trigger-the-activity-bar.md) (permitting structured progress reporting for long-running slash commands without polluting round liveness)

---

## Context and Problem Statement

In current operations, invoking the `/compact` slash command in the terminal UI overwhelmingly fails with the opaque error:
```text
Not enough complete rounds to compact.
```

Investigation reveals a severe impedance mismatch between **passive auto-compaction** and **active user-driven compaction**:

1. **Category Mistake of Compaction Target Sizing**:
   Auto-compaction (ADR-0283) operates as a passive runaway backstop: when pressure hits the 85% high watermark, it recovers down to `target_utilization = 0.25` (25% window, e.g. 50,000 tokens on a 200k model). However, the `/compact` slash command erroneously reuses this exact auto-compaction budget. When a user with 15 turns and 18,000 tokens requests a compaction, `find_cut_point_nodes` observes `total_tokens <= target_tokens` and unconditionally aborts, claiming no compaction is needed.
2. **The "Black-Box Compaction" Cognitive Gap**:
   When compaction succeeds, `RoundEvent::Compacted` emits only numerical statistics (`archived_messages`, `tokens_before`, `tokens_after`). The resulting transcript notice is a sterile single-line text: `Compacted 12 messages: 45000 -> 3200 tokens.`. The actual synthesized knowledge—goals, decisions, modified files, and architectural constraints—remains completely hidden from the user. Users cannot audit what the model retained or hallucinated.
3. **Execution Blind Spot (Frozen TUI)**:
   Per ADR-0110, slash commands do not light up the round activity bar to avoid corrupting round state. However, generative summarization requires 2–6 seconds of LLM inference. During this time, the TUI appears completely frozen without progress indication.

---

## Decision Drivers

- **Zero-Barrier Intent Execution**: An explicit user command (`/compact`) is an imperative intention; it must never be gated by passive token capacity thresholds.
- **The $N-1$ Tail-Preserving Invariant**: Compaction must fold historical rounds $[0 \dots N-1]$ while preserving the latest volatile round $N$ verbatim as a conversational anchor, eliminating context disorientation.
- **Auditable Belief States (White-Box UX)**: Compaction summaries and tracked artifacts must be first-class, inspectable UI elements in the transcript.
- **Interactive TUI Affordance**: Compaction entries must support collapsible inspection via `Space` / `Enter`, mirroring tool call card ergonomics.
- **Clean Architecture & No Legacy Burden**: Extend `RoundEvent::Compacted` and `ContextProjectionCheckpoint` directly; do not maintain dual-track ad-hoc text channels.

---

## Decision Outcome

### 1. Dual Slicing Policy: Passive vs. Active Compaction

We introduce explicit slicing disciplines in `CausalCompactor`:

- **Auto-Compaction (Passive Relief)**: Preserves `target_tokens` (25% window) to maximize KV-cache reuse during sustained agentic autonomous loops.
- **Manual / Intent Compaction (Active Epoch Advancement)**:
  - Folds all completed lineage groups prior to the latest active round ($N-1$).
  - Preserves the latest round ($N$) intact as the conversation anchor.
  - Requires only that at least one completed round exists before the tail.
  - Accepts optional user guidance via `/compact [notes]` which is injected directly into `extra_context` to steer the generative summarizer.

### 2. Event Model & Checkpoint Payload Expansion

`ContextProjectionCheckpoint` and `RoundEvent::Compacted` are enhanced to carry the authoritative summary and artifact manifest:

```rust
pub struct ContextProjectionCheckpoint {
    pub operation: ContextProjectionKind,
    pub archived_messages: usize,
    pub active_messages: usize,
    pub window_tokens_before: usize,
    pub window_tokens_after: usize,
    pub summary: Option<String>,
    pub tracked_files: Vec<String>,
}

pub enum RoundEvent {
    // ...
    Compacted {
        archived_messages: usize,
        window_tokens_before: usize,
        window_tokens_after: usize,
        summary: Option<String>,
        tracked_files: Vec<String>,
    },
}
```

### 3. TUI Inspectable Checkpoint Card (`TranscriptMessage::CompactedCard`)

`mutx` renders compaction checkpoints as interactive, foldable cards:

- **Collapsed State (Default)**:
  ```text
  ✦ Context Compacted  •  Folded 14 turns  •  48.2k → 3.1k tokens (-93%)  •  [Enter to inspect]
  ```
- **Expanded State**:
  Renders a structured, border-framed panel containing:
  - **Objective & Architectural Constraints**
  - **Tracked Files** (files read, edited, or created across folded rounds)
  - **Synthesized Dialogue Summary**
  - Toggleable instantly via `Space` or `Enter`.

### 4. Progress Observability Without Round Liveness Corruption

During `/compact` execution:
1. `record_ack` or an explicit ephemeral command progress message is rendered in the transcript immediately:
   `⠋ Compacting context (summarizing completed rounds)...`
2. Upon completion, the ephemeral entry resolves cleanly into the durable `CompactedCard`.
3. If no completed prior rounds exist (e.g., fresh session with 0 completed turns), the command responds deterministically:
   `"No completed dialogue history to compact yet."`

---

## Invariants & Behavioral Boundaries

- **`[INV-COMPACT-07]` Zero Threshold on Explicit Invocation**: Manual `/compact` must never inspect `compaction_threshold_tokens` (85%) or reject execution based on token count when fold candidates exist.
- **`[INV-COMPACT-08]` Tail Turn Integrity**: Manual compaction must never split the innermost active conversational round between user input and pending/final assistant response.
- **`[INV-COMPACT-09]` Transparent Auditability**: The exact summary string and tracked files injected into the compiled Session IR must be conveyed verbatim to the client in `RoundEvent::Compacted`.
- **`[INV-COMPACT-10]` Terminal Independence**: If LLM summarization fails during `/compact`, the engine must fall back to deterministic causal excerpt extraction (ADR-0255), never leaving the session in an uncompacted broken state.

---

## Rejected Alternatives & Negative Knowledge

### Requiring 50% Window Pressure Before Permitting `/compact`
- **Why considered**: Prevent users from compacting too frequently and incurring LLM summarization costs.
- **Why rejected**: Violates user agency and command contract. When a user finishes a major milestone and wishes to start a clean phase, forcing them to accumulate irrelevant tokens is hostile and counterproductive.

### Slicing 100% of Context (Zero Tail Preservation)
- **Why considered**: Maximizes token reclamation by summarizing the entire transcript including the latest assistant message.
- **Why rejected**: Catastrophic conversational discontinuity. If the assistant asked a clarifying question or proposed two options in turn $N$, wiping turn $N$ leaves the user's next message ("let's go with option 2") referencing non-existent context in the active prompt.

### Plain Text Markdown Rendering Without Collapsible State
- **Why considered**: Simpler implementation using standard transcript notices.
- **Why rejected**: In long sessions, a multi-paragraph summary card dumping into the transcript clutters the viewport. A foldable card provides clean visual ergonomics while keeping full details one keystroke away.

---

## Positive Consequences

- Users can cleanly trigger `/compact` at any point after round 1.
- Complete visibility into model working memory and tracked file state.
- Smooth transition between conversational phases with zero context amnesia.
