# 0167. Worker-Station Agent Model and Hypervisor Station Placement

- **Status:** Accepted
- **Date:** 2026-09-12
- **Supersedes/Refines:** ADR-0144, ADR-0150

## Context

Prior architectural decisions (ADR-0144 and ADR-0150) unified terminology under vertical agent tiers (`Supervisor < Master < Runner`) and introduced `Steward` as an out-of-band "cognitive attendant" with persona offices.

However, two fundamental architectural conflations existed:

1. **Conflating Archetype with Station Placement:**
   - A daemon-level coordinator (`Supervisor`) and a session-level agent (`Master`) are identical in archetype: both run a full Agentic Loop (inference, intent reasoning, memory, tool dispatch). They only differ in the **station** they occupy and the **toolset** assigned to them.
   - Forcing them into distinct vertical agent types resulted in synthetic hierarchy lattices, `AgentTier` ordering logic, and duplicate runtime machinery.

2. **Persona Leakage in Internal Infrastructure:**
   - Internal cognitive operations (stream-loop sentinel verification, context compaction, session titling, working-memory digests) are stateless, zero-tool, single-shot LLM transformations executed as internal mechanics of the Agent Harness.
   - Dressing these operations up as an agent ("Steward") with an artificial "Office" persona caused conceptual clutter and semantic confusion.

## Decision

### 1. The Worker-Station Model (Strict Orthogonality)

We decouple the system into two orthogonal planes: **Agent Archetypes (Workers)** and **Host Stations (Placements)**:

```text
┌─────────────────────────────────────────────────────────────┐
│                 1. Agent Archetypes (Workers)               │
│                                                             │
│       [ Master ] (Driving Brain)      [ Runner ] (Worker)   │
│       - Full Agentic Loop             - Single-task worker  │
│       - Intent & tool authority       - Sandboxed/isolated  │
└──────────────────────────────┬──────────────────────────────┘
                               │
                    Placed into / Occupies
                               │
┌──────────────────────────────▼──────────────────────────────┐
│                 2. Host Stations (Placements)               │
│                                                             │
│  [ Hypervisor Station ] ──> Staffed by a Master agent       │
│  - Host: Daemon singleton                                   │
│  - Responsibilities: Cross-session orchestration, 联调      │
│                                                             │
│  [ Session Station ]    ──> Staffed by a Master agent       │
│  - Host: Session instance                                   │
│  - Responsibilities: User conversation, workspace execution │
│                                                             │
│  [ Subtask Station ]    ──> Staffed by a Runner agent       │
│  - Host: Session child execution                            │
│  - Responsibilities: Read-only research, tests, MCP calls   │
└─────────────────────────────────────────────────────────────┘
```

1. **`AgentKind`** (Strictly two archetypes):
   - `AgentKind::Master`: The driving brain with full ReAct loop, tool resolution, and memory.
   - `AgentKind::Runner`: Mission-scoped, isolated, short-lived task execution worker.

2. **`MeshStation` / `MeshAddress`** (Three operational placements):
   - `MeshStation::Hypervisor`: Daemon-level coordinator (`MeshAddress::hypervisor()`).
   - `MeshStation::Session`: Session-level primary brain (`MeshAddress::master()`).
   - `MeshStation::Subtask`: Session subtask runner (`MeshAddress::runner()`).

3. **Routing Lawfulness**:
   - `Instruction` flows top-down (`Hypervisor -> Session`, `Session -> Subtask`).
   - `Report` / `ProgressNote` / `RunnerEol` flow bottom-up (`Subtask -> Session`, `Session -> Hypervisor`).
   - `PeerNote` flows strictly within the same station level (`Session <-> Session`, `Subtask <-> Subtask`).

### 2. Harness Cognitive Pipeline (De-stewarded)

We completely eliminate the `Steward` persona and "Offices":
- Replaced by typed **`CognitiveTask`** contracts in `muta-contracts::cognitive` and executed by `CognitivePipeline` in `muta-agent::cognitive`.
- Zero persona baggage, typed schema parsing, timeout enforcement, and non-negotiable **fail-open** guarantees.

## Consequences

- **Extreme Conceptual Purity**: Only two agent kinds (`Master` / `Runner`), one daemon governance station (`Hypervisor`), and pure stateless cognitive utilities (`CognitivePipeline`).
- **Complete Elimination of Dead Concepts**: Deprecated `AgentTier`, `StewardOffice`, and `steward_identity`.
- **High Code Reuse**: The Hypervisor directly reuses the standard `Master` agent engine, eliminating duplicate runtime abstractions.
- **Fail-open Resilience**: Cognitive reviews remain bounded, non-blocking, and fail-open.
