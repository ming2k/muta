# 0185. Non-Blocking Workspace Trust Pipeline, Single-Writer Security Attestation, and Projection-Preserving Interrupted Turn Architecture

- **Status:** Accepted
- **Date:** 2026-09-16
- **Builds on:** ADR-0127 (durable round-interrupt records), ADR-0175 (pre-attach workspace trust interstitial), ADR-0178 (persistent single-writer actor)

## Context

Three interrelated architectural bottlenecks and usability gaps emerged in the interaction and persistence pipeline:

1. **Workspace Trust UI Stall and Storage Lock Contention**:
   - When entering a quarantined workspace, `mutx` mounts the `PreAttach` trust gate (ADR-0175). Upon user confirmation ("Trust workspace"), the UI enters a `submitting` state (`Trusting workspace... / Enabling configurations and entering session...`).
   - In previous iterations, `/trust` dispatched synchronously inside the runtime loop, executing blocking MCP process spawns and skills scanning inline. Even after moving heavy reconfiguration to background tasks, `crates/muta-persistence/src/workspace_security.rs` still bypassed the Single-Writer Actor (`PersistenceHandle`, ADR-0178) by opening an ad-hoc SQLite connection directly (`DatabaseEngine::open`), resulting in SQLite lock contention and up to 5-second `busy_timeout` stalls.
   - Furthermore, recursive SHA-256 digest computation over skills, hooks, and rule assets was executed synchronously on Tokio worker threads without thread-pool offloading.
   - During this window, `PreAttachState` unconditionally swallowed all keyboard input (including `Esc`), trapping the operator in an unescapable screen if any stall occurred.

2. **Dangling Turn Amnesia on Session Interrupt and Reload**:
   - In accordance with LLM message schema strictness, in-flight streamed model responses cannot be persisted into `model_window` mid-stream without risking broken JSON tool-call syntax or orphaned tool results that cause API 400 Bad Request errors on subsequent turns.
   - However, when a round was interrupted (via `[Esc Esc]`, client disconnect, or fatal error), the in-flight streamed text was completely discarded (`StreamDiscard`). Upon session resume or reload, the operator observed only their own prompt followed immediately by `▲ interrupted`, losing all assistant reasoning or drafted text generated prior to the interruption.

3. **Round Counter Attribution Off-by-One in Interrupt Records**:
   - `start_interactive_round` captured `round_at_admission` before `execute_round` invoked `agent.bump_round()`. Consequently, when Round $N$ was interrupted, the generated `RoundInterrupt` record mistakenly claimed `round: Some(N - 1)`, resulting in confusing UI banners (e.g., displaying `Round 2 — cancelled via [Esc Esc]` under `< round 3`).

## Decision

We execute an uncompromising, clean-break overhaul of the trust attestation pipeline and interrupt projection lifecycle:

### 1. Single-Writer Security Attestation and Non-Blocking Attestation Offload
- **Eliminate Ad-hoc SQLite Connection in `WorkspaceSecurityStore`**:
  All persistence of `state:workspace_security` is delegated to the authoritative `PersistenceHandle` (via `set_json_blocking` on the dedicated `"muta-persistence-writer"` thread). Direct `DatabaseEngine::open` writes are completely eliminated, banishing lock contention and `busy_timeout` stalls.
- **Fail-Safe PreAttach Cancellation**:
  `PreAttachState::apply` explicitly permits `QuestionAction::Cancel` while in `submitting` state. If an external subsystem or network delay stalls trust resolution, the operator can cleanly press `[Esc]` to abort and keep the workspace quarantined, eliminating indefinite UI lockup.

### 2. Clean Interrupt Semantics and Eradication of Abandoned Draft Repeats
- **Strict Decoupling of Model Memory vs. Stop Events**:
  - The model-visible conversation transcript (`model_window` in `SessionData`) remains strictly clean: partial, unclosed, or invalid assistant turns never enter `model_window`.
  - An interruption (`User`, `Superseded`, `Terminated`) cleanly marks the turn boundary without polluting the event notification or SQLite with discarded drafts.
- **`RoundInterrupt.detail` Reserved Strictly for Error Diagnostics**:
  - `RoundInterrupt.detail` is strictly populated when `reason == RoundInterruptReason::Error` (e.g. fatal provider HTTP errors, rate limit exhaustion, network failure payload).
  - For non-error interrupts (`User`, `Superseded`, `Terminated`), `RoundInterrupt.detail` is `None`. The TUI `TranscriptMessage::round_interrupted` creates a clean, self-contained notice row without repeating the aborted text underneath.
- **Rejected Pattern: In-Flight Draft Capture into `RoundInterrupt.detail` (Superseded & Eradicated)**:
  - Storing in-flight streaming assistant deltas into an `Arc<Mutex<String>>` draft accumulator and writing them into `RoundInterrupt.detail` was attempted and rejected.
  - *Why it failed*:
    1. **Live View Duplication**: During streaming, the assistant's partial prose is already rendered in the active transcript above the interrupt marker. Re-rendering `detail` in the notice banner duplicated the entire aborted output verbatim.
    2. **Contradiction of User Intent**: When an operator presses `[Esc Esc]`, they explicitly intend to abort/discard the output. Regurgitating the aborted output inside the notification banner added noise and clutter.
    3. **Streaming Hot-Path Overhead**: Grabbing a mutex and pushing to an accumulator on every single provider delta added lock contention and allocations across streaming turns.
    4. **Persistence Bloat**: In-flight drafts permanently bloated SQLite `sessions.data` interrupt records with discarded data.

### 3. Exact Round Counter Admission
- In `start_interactive_round`, `round_at_admission` is derived from `input.driver`:
  - For `RoundDriver::Fresh`: `context.agent.round_count().saturating_add(1)`.
  - For `RoundDriver::Resume { point }`: `point.round`.
- The interrupted round attribution strictly matches the active round header.

## Consequences

### Positive
- **Zero-Stall Workspace Trust**: Trusting a workspace is instantaneous: single-writer SQLite delegation eliminates file lock thrashing, and unblocked event dispatch delivers instant transition to session view.
- **Fail-Safe UI Resilience**: Operators are never trapped in a submission screen.
- **Clean Interrupted Experience**: Interrupt markers communicate the exact stop reason without regurgitating aborted draft prose; no redundant noise or duplicate text in the transcript.
- **Zero Streaming Overhead**: Provider delta streaming runs lock-free without draft accumulator mutex acquisition.
- **Compact Persistence**: SQLite interrupt records do not bloat with abandoned draft payloads.
- **Accurate Telemetry & Audit**: Round numbers on interrupt records agree with session transcript headers.

### Negative / Neutral
- None. Abandoned drafts from intentionally aborted turns are discarded without residue.
