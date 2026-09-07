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

### 2. Projection-Preserving Interrupted Turn Architecture
- **Strict Decoupling of Model Memory vs. Projection Ledger**:
  - The model-visible conversation transcript (`model_window` in `SessionData`) remains strictly clean: partial, unclosed, or invalid assistant turns never enter `model_window`.
  - The human-visible projection ledger preserves the accumulated in-flight stream text inside `RoundInterrupt.detail`.
- **Durable Draft Capture**:
  - In `orchestration.rs`, an in-flight draft accumulator captures live streamed assistant text deltas during turn execution.
  - Upon interruption (`HarnessError::Interrupted`), the accumulated draft is trimmed and stamped directly into `RoundInterrupt.detail`.
  - In TUI `TranscriptMessage::round_interrupted`, `NoticeParts.detail` reflects `record.detail`, allowing `draw_notice_view` to render the abandoned draft seamlessly beneath the interrupt marker (`▲ interrupted`).
  - Upon session resume, the operator sees exactly what the assistant was generating before interruption, resolving the perceived "database loss" while maintaining 100% clean LLM context.

### 3. Exact Round Counter Admission
- In `start_interactive_round`, `round_at_admission` is derived from `input.driver`:
  - For `RoundDriver::Fresh`: `context.agent.round_count().saturating_add(1)`.
  - For `RoundDriver::Resume { point }`: `point.round`.
- The interrupted round attribution strictly matches the active round header.

## Consequences

### Positive
- **Zero-Stall Workspace Trust**: Trusting a workspace is instantaneous: single-writer SQLite delegation eliminates file lock thrashing, and unblocked event dispatch delivers instant transition to session view.
- **Fail-Safe UI Resilience**: Operators are never trapped in a submission screen.
- **Lossless Interrupted Experience**: In-flight assistant thoughts and drafts survive interruptions and reloads in the projection view without risking LLM context corruption.
- **Accurate Telemetry & Audit**: Round numbers on interrupt records agree with session transcript headers.

### Negative / Neutral
- `RoundInterrupt` records with draft details occupy slightly more storage in SQLite `sessions.data` (typically a few hundred bytes of text), which is negligible.
