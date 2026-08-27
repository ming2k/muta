# 0150. Two-Axis Agent Architecture and the Harness Steward

- **Status:** Accepted
- **Date:** 2026-08-27
- **Extends/Refines:** ADR-0144

## Context

ADR-0144 unified the agent model under a single vocabulary with three vertical tiers: `Supervisor` (tier 0), `Master` (tier 1), and `Runner` (tier 2). While that unified terminology, two fundamental limitations emerged in practice:

1. **One-dimensional depth obscures actual coupling:**
   - `Master` and `Runner` are tightly coupled: they form the **Operational Production Core**, sharing session state, delegating file/command operations, and binding life cycles (replacing a Master reclaims its Runners).
   - `Supervisor` is the **Fleet & Session Control Plane**: a daemon-level singleton loosely coupled with Masters via asynchronous Mesh messages for cross-session tracking and joint debugging (联调).
   - Modeling these as a simple linear chain (`Supervisor < Master < Runner`) misrepresents their operational relationship.

2. **Internal cognitive infrastructure had no architectural home:**
   - Critical harness-internal tasks — semantic doom-loop detection, sanity/safety verification of strings/actions, context compaction, and session titling — are stateless, single-shot, zero-tool cognitive operations.
   - Forcing these into the `Runner` pool (such as `RUNNER_TITLE`) or scattering them as ad-hoc prompt snippets in utility modules caused semantic leakage and cluttered the mission-oriented runner catalog.

## Decision

### 1. Two Axes, Three Planes

The Muta agent architecture is partitioned into two orthogonal axes and three planes:

```text
┌─────────────────────────────────────────────────────────────┐
│             Fleet & Session Control Plane                   │
│         Supervisor (Daemon Singleton / Mesh Root)           │
└──────────────────────────────┬──────────────────────────────┘
                               │ (Loose Mesh Messaging)
┌──────────────────────────────▼──────────────────────────────┐
│             Operational Production Core                     │
│         Master (Session Brain) <═══> Runner (Field Worker)  │
│         - User Conversation          - Autopilot Execution  │
│         - Toolset Authority          - Workspace Scoping    │
└──────────────────────────────┬──────────────────────────────┘
                               │ (Runs within)
┌──────────────────────────────▼──────────────────────────────┐
│             Harness Infrastructure Plane                    │
│         Agent Harness Base                                  │
│           └── Steward (Harness Cognitive Attendant)         │
│               - Semantic Loop Sentinel                      │
│               - Sanity & Safety Verifier                    │
│               - Context Compactor                           │
│               - Session Titler                              │
└─────────────────────────────────────────────────────────────┘
```

- **Operational Core (`Master` + `Runner`)**: Production actors that interact with the user and execute side-effecting tools in the workspace.
- **Fleet Control Plane (`Supervisor`)**: Macro orchestration across multiple sessions, tracking global tokens and coordinating multi-session workflows.
- **Harness Infrastructure Plane (`Steward`)**: Out-of-band, stateless, zero-tool cognitive attendant serving the Agent Harness state machine.

### 2. Typed Cognitive Contracts (`StewardTask`)

All Steward tasks are defined as strongly-typed cognitive contracts in `crates/muta-contracts/src/steward.rs`:

```rust
#[async_trait]
pub trait StewardTask: Send + Sync {
    type Input: Serialize + Send + Sync;
    type Output: DeserializeOwned + Send + Sync;

    fn name(&self) -> &'static str;
    fn system_prompt(&self) -> &'static str;
    fn render_prompt(&self, input: &Self::Input) -> String;
    fn model_preference(&self) -> StewardModelPreference { StewardModelPreference::FlashLite }
    fn timeout_ms(&self) -> u64 { 2000 }
}
```

Four standard tasks are provided out-of-the-box:
1. `SemanticLoopSentinelTask` -> `SemanticLoopVerdict` (includes `remedy_nudge` for prescriptive self-correction).
2. `SanityVerifierTask` -> `SanityCheckVerdict` (evaluates rationality and safety risk).
3. `SessionTitlerTask` -> `SessionTitleOutput` (clean 3-7 word title).
4. `TranscriptCompactorTask` (context compression and projection).

### 3. Non-negotiable Performance & Reliability Invariants

1. **Tiered Execution (0ms Fast-path + On-demand Steward)**: Deterministic L1 heuristics (e.g. signature hash in `DoomGuard`) handle common cases at 0ms cost. Only when ambiguous or oscillating patterns are detected is the L2 semantic Steward invoked.
2. **Fail-Open by Default**: Steward failures, timeouts, or unparseable outputs fall back immediately to safe defaults without blocking the user or production loop.
3. **Resource & Model Isolation**: Steward defaults to lightweight, low-latency models (`FlashLite`) and accounts tokens separately from user session budgets.

### 4. Deprecating `RUNNER_TITLE`

`RUNNER_TITLE` is removed from the Runner preset pool (`RunnerPresetPool`). Session titling is now natively driven by `Agent::steward().generate_title()`. The Runner concept is restored to its true mission: autonomous task workers with tools (`RUNNER_EXPLORE`, `RUNNER_CODE`, `RUNNER_MCP_SPECIALIST`).

## Consequences

- **Architectural Clarity**: Clear separation between user-facing production actors (`Master`/`Runner`), daemon governance (`Supervisor`), and internal cognitive operations (`Steward`).
- **Clean Contracts**: Eliminates ad-hoc prompt strings; all internal LLM calls are typed, schema-validated, and timeout-bounded.
- **Resilience**: Fail-open guarantees prevent harness-internal tasks from stalling user turns.
- **Prescriptive Guidance**: Loop detection provides actionable remedial nudges rather than blind blocks.
- **Cost**: A dedicated `steward` module in `muta-contracts` and `muta-agent`.
