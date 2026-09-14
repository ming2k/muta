# 0247. Trajectory Loop Guard: Unified Cognitive Arbitration and Escalating Backoff Ladder

- Status: Proposed
- Date: 2026-04-14
- Scope: core/agent, harness/steward, contracts
- Deciders: Core Architecture Team
- Consulted: Agent Runtime Team
- Informed: All Contributors
- Extends/Refines: ADR-0113, ADR-0148, ADR-0150
- Amends: ADR-0034

---

## Context and Problem Statement

Muta historically addressed agent loop prevention through fragmented and informally named mechanisms:
1. **Turn-internal generation looping** was addressed via `StreamLoopDetector` and L2 `StreamSentinel` cognitive verification (cutting degenerate repetitive tokens in-flight).
2. **Turn-external tool-dispatch looping** was addressed via `DoomLoopGuard` (ADR-0113, ADR-0148), a pre-dispatch deterministic signature bookkeeper enforcing a static occurrence threshold (default 3) within a sliding window.

While effective against primitive variants (`sleep 1; make`), this split and terminology introduce significant limitations:
- **Informal vocabulary**: The term "doom loop" reflects informal industry jargon rather than the formal concept of **Agent Interaction Trajectories** ($\tau = (s_0, a_0, r_0, \dots)$). Modern agent architectures require inspecting broader behavioral patterns (multi-tool thrashing, cyclic state oscillation) under a unified **Trajectory Loop Guard**.
- **Asymmetric decision pipelines**: Turn-internal looping uses a dual-layer pipeline (L1 mechanical heuristic $\to$ L2 cognitive arbitration), whereas Turn-external tool calls rely entirely on static string signature counting. Rigid signature counting cannot distinguish between legitimate repetitive workflows (e.g. repeated test runs during incremental debugging, paginated data inspection) and genuine cognitive paralysis.
- **Binary failure modes**: Once the static threshold is breached, tools are blocked immediately without assessing progress, leading to developer frustration and false-positive blocks during legitimate high-frequency iterations.

## Decision Drivers

- **Domain rigor**: Replace informal jargon with formal reinforcement learning and agentic trajectory terminology (`TrajectoryLoopGuard`).
- **Pipeline symmetry**: Unify Turn-internal (streaming tokens) and Turn-external (tool dispatch) loop guards under an identical architectural pattern: `L1 Heuristic Probe` $\to$ `L2 Cognitive Arbiter` $\to$ `Action / Backoff`.
- **False-positive resilience**: Eliminate false positives in legitimate repetitive tasks (e.g., test-driven development, regression sweeps) without sacrificing loop termination guarantees.
- **Fail-open safety**: Ensure that cognitive review failures (provider timeouts, token quota exhaustion) never deadlock or block user work.
- **Backward compatibility**: Preserve non-breaking backward compatibility with existing TOML configuration keys (`[agent.doom_guard]`, `[master.doom_guard]`).

## Considered Options

- **Option 1**: Retain static `DoomLoopGuard` and merely increase default thresholds (e.g., threshold 5 or 8).
- **Option 2**: Pure LLM-evaluated loop detection on every turn (discarding deterministic signatures).
- **Option 3**: **Unified Trajectory Loop Guard with Deterministic L1 Screening, Steward L2 Cognitive Arbitration, and an Escalating Backoff Ladder (4 $\to$ 8 $\to$ 12).**

## Decision Outcome

Chosen option: **Option 3**.

### 1. Architectural Reconceptualization: `TrajectoryLoopGuard`

The pre-dispatch tool guard is elevated from a tool-name filter to an Agent Trajectory Supervisor.
- `DoomLoopGuard` is refactored into `TrajectoryLoopGuard` in `crates/muta-agent/src/trajectory_guard.rs`.
- The configuration table `[agent.trajectory_guard]` becomes canonical; `[agent.doom_guard]` and `[master.doom_guard]` remain supported as deserialize aliases.
- The guard inspects the sequence of dispatched tool calls, arguments, and outcome deltas as a connected trajectory rather than isolated commands.

### 2. Symmetrical Dual-Tier Pipeline

Both Turn-internal and Turn-external loop defenses adhere to the identical two-tier contract:

```text
┌─────────────────────────────────────────────────────────────────────────────┐
│                      Unified Loop Defense Architecture                      │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                             │
│  [Turn-Internal: Token Stream]             [Turn-External: Tool Trajectory] │
│                 │                                          │                │
│                 ▼                                          ▼                │
│  L1: KMP Border & Dwell Trail              L1: Normalized Signature Window  │
│  (Pure algorithmic, zero I/O)              (Pure hash matching, zero I/O)   │
│                 │                                          │                │
│                 └──────────────┬───────────────────────────┘                │
│                                │                                            │
│                     Trips Current Ladder Tier                               │
│                                │                                            │
│                                ▼                                            │
│              L2: Steward Cognitive Arbitration Task                         │
│              (Stateless, single-shot, bounded LLM review)                   │
│                                │                                            │
│                ┌───────────────┴───────────────┐                            │
│                ▼                               ▼                            │
│        [Confirmed Loop]                [Productive Work]                    │
│                │                               │                            │
│    Tiered Intervention:                Escalating Backoff Ladder:           │
│    1. Surgical Signature Block         Advance threshold: 4 -> 8 -> 12      │
│    2. Guided Steering Nudge            Reset strike accumulator             │
│    3. Terminal Round Abort (if stuck)  Continue uninterrupted               │
│                                                                             │
└─────────────────────────────────────────────────────────────────────────────┘
```

### 3. Escalating Backoff Ladder (4 $\to$ 8 $\to$ 12)

To balance zero overhead on normal iterations with decisive intervention on runaways, the deterministic L1 heuristic adopts an escalating threshold ladder:
- **Base Threshold (Tier 1 = 4)**: A normalized signature appearing 4 times within the sliding window triggers an L2 cognitive arbitration request to the Harness Steward (`StewardTask::ReviewTrajectoryLoop`).
- **Cognitive Acquittal & Backoff (Tier 2 = 8)**: If the Steward concludes the agent is making meaningful progress (e.g. error messages are changing, test suites are progressing), the signature is acquitted and the threshold escalates to 8.
- **Ceiling Backoff (Tier 3 = 12)**: If acquitted a second time, the threshold escalates to 12. 12 serves as the non-escalating terminal review boundary for the remainder of that user round.
- **Fail-Open Invariant**: If the Steward task errors out or times out (2.0s SLA), it defaults to `Verdict::NoLoop` and advances the backoff ladder, preventing harness deadlocks.

### 4. Graduated Enforcement: Surgical Masking Before Terminal Abort

When the Steward confirms a trajectory loop (`Verdict::Loop`):
1. **First Confirmation (Surgical Intervention)**:
   - The offending signature is inserted into `RoundGuardState.blocked_signatures`.
   - The tool is short-circuited pre-dispatch and returns a structured `[trajectory guard] blocked` result explaining why re-execution is forbidden.
   - An anti-anchoring system directive is injected into the transcript, directing the model to alter its strategy or call `abort`.
2. **Second Confirmation / Exhaustion (Hard Stop)**:
   - If the model repeatedly ignores the steering injection or all viable tools become masked, the harness terminates the user round with a clear diagnostic notice (`HarnessError::TrajectoryLoopExhausted`).

### Invariants & Behavioral Boundaries

- **`[INV-LOOP-01]` Pre-Dispatch Sovereignty**: No side-effecting tool identified by the L1 filter shall execute while an L2 cognitive review is pending. L2 trajectory reviews run bounded within pre-dispatch gating.
- **`[INV-LOOP-02]` Fail-Open on Infrastructure Error**: Steward cognitive arbitration failures (network timeouts, rate limits, malformed JSON) must never crash or block the turn loop; they must fail open, log a warning, and increment the backoff ladder.
- **`[INV-LOOP-03]` Per-Round Lifecycle Containment**: All trajectory signature masks and backoff ladder states are scoped to `RoundState`. They are reclaimed upon user round termination and never leak across rounds.
- **`[INV-LOOP-04]` Backward Compatibility Guarantee**: Existing configurations using `[master.doom_guard]` or `[agent.doom_guard]` must parse seamlessly without deprecation warnings in CLI or logs.

### Positive Consequences

- Eliminates false-positive blocks during valid repetitive workflows (e.g. rapid test cycles, paginated searches).
- Replaces gaming/workarounds (`sleep` jitter) with semantic comprehension of progress.
- Unifies internal cognitive sentinel patterns across both execution layers.
- Establishes academic and professional consistency across the codebase and documentation.

### Negative Consequences & Trade-offs

- **Cognitive Latency**: When an L1 threshold is tripped, pre-dispatch incurs a ~500ms–2000ms latency pause while the Steward evaluates the trajectory.
  - *Mitigation*: The threshold ladder (4 $\to$ 8 $\to$ 12) guarantees that legitimate fast loops trigger review at most twice before reaching the ceiling.
- **Steward Token Overhead**: Reviewing the trajectory snippet consumes tokens on the background harness provider.
  - *Mitigation*: The review payload projects only recent trajectory signatures and diff digests, capped at 4,000 characters.

## Rejected Alternatives & Negative Knowledge

### Option 1: Static Threshold Increase without AI Review
- *Why considered*: Minimal implementation cost; zero additional provider latency.
- *Why rejected*: Simply bumping the static threshold from 3 to 6 or 8 merely delays runaway loops without preventing them, while still failing on 9th valid repetitions. It preserves the fundamental rigidity of deterministic counting.

### Option 2: Pure Cognitive Arbitration on Every Turn
- *Why considered*: Maximum semantic comprehension of every step.
- *Why rejected*: Unacceptable latency and cost. Running an LLM review before every tool dispatch adds thousands of milliseconds and doubles token burn. The deterministic L1 heuristic is mandatory as a zero-cost gatekeeper.

## Links

- Related ADRs:
  - [ADR-0034](0034-range-aware-pruning-and-deterministic-read-loop-guard.md) — Range-aware pruning and deterministic read-loop guard
  - [ADR-0113](0113-task-supervision-and-idle-suspension.md) — Task supervision and normalized loop signatures
  - [ADR-0148](0148-doom-guard-threshold-and-content-aware-mutation-signatures.md) — Doom-guard threshold knob and content-aware mutation signatures
  - [ADR-0150](0150-two-axis-agent-architecture-and-harness-steward.md) — Two-axis agent architecture and harness steward
