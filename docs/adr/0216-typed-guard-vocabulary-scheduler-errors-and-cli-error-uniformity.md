# 0215. Typed Guard Vocabulary, Scheduler Errors, and CLI Error Uniformity

- **Status:** Accepted
- **Date:** 2026-02-14

## Context

Three consistency debts blocked readers and left error semantics collapsed
into bare strings:

1. **Guard vocabulary lived in a misnamed module.** The post-hoc
   `ReadLoopGuard` was replaced by the pre-dispatch `DoomLoopGuard`
   (ADR-0148), but the shared vocabulary — `GuardAction`, `RoundGuardState` —
   remained in `loop_guard.rs`. The module name described neither its content
   nor its role, and every doc comment carried a historical comparison
   against a deleted type.

2. **`ToolScheduler` collapsed four failure shapes into one `String`.** The
   scheduler's result channel (`Result<R, String>`) carried four
   semantically distinct outcomes: queued-then-cancelled (`"cancelled before
   start"`), running-then-cancelled (`"cancelled"`), a caught panic
   (`"tool task panicked: …"`), and task-level detail — all distinguished,
   if at all, by string matching. The dispatch layer could not tell *why* a
   call produced nothing without fragile `contains` checks.

3. **The CLI mixed two error types.** `crates/muta` returned
   `Result<(), String>` from `status::run`, `detach_daemon`, and
   `stop_daemon` while every other entry point returned
   `Result<(), Box<dyn std::error::Error>>`; the mismatch was papered over
   with `.map_err(Into::into)` at three call sites.

Additionally, `context_projection` was reviewed for rename to
"compaction" and rejected (see Alternatives).

## Decision

1. **Rename `loop_guard` → `guard`** (break clean, no alias):
   `crates/muta-agent/src/guard.rs` now holds `GuardAction` and
   `RoundGuardState`; every reference site
   (`agent/mod.rs`, `agent/rounds.rs`, `dispatch_pipeline.rs`,
   `doom_guard.rs`, `lib.rs` re-exports) points at `crate::guard::*`.
   The doom-guard module doc no longer narrates the deleted
   `ReadLoopGuard`; it states the pre-dispatch rationale directly.
   `muta-contracts::message.rs` fixes its cross-crate doc pointer to
   `muta_agent::guard`.

2. **Type the scheduler's error channel** with
   `crate::tool_scheduler::SchedulerError`:

   - `CancelledBeforeStart` — the task was still queued when the batch was
     cancelled; it never ran.
   - `Cancelled` — the task observed the token and gave up before finishing.
   - `Panicked(String)` — the task's future panicked; the scheduler caught
     it via `catch_unwind` so the queue re-scan could proceed.

   The channel carries **scheduler-level failures only**. A task's own
   failure is its result, not the scheduler's business: for tool dispatch
   it already flows through the normal `ToolResult` event. The former
   fourth shape (task failure as an error string) was therefore *dropped*,
   not renamed — folding it back into the error channel would have
   preserved the original ambiguity.

3. **Unify CLI errors on `Box<dyn std::error::Error>`.** The three
   `Result<(), String>` signatures become
   `Result<(), Box<dyn std::error::Error>>`; the three
   `.map_err(Into::into)` adapters at the call sites are deleted. The
   standard-library `From<String> for Box<dyn Error>` impl carries the
   conversion, so `?` bridges freely and no new error type is imposed on a
   thin presentation layer.

4. **Keep `context_projection` named as-is.** The rename review concluded
   the "projection" umbrella is correct: the gate governs *any*
   model-visible window replacement between rounds — prune, compact, and
   future strategies — while `compaction` names one specific strategy (and
   already names a module). Renaming the umbrella to one of its
   strategies would misstate the seam.

## Alternatives considered

- **Keep `loop_guard` and add a `pub use guard as loop_guard` alias.**
  Rejected: an alias preserves the misname forever and teaches new readers
  the wrong noun. No external consumer depends on the module path.

- **A `thiserror` domain error for the CLI.** Rejected for this layer: the
  binary prints and exits; it does not match on error kinds. A typed enum
  would add a variant per message with zero readers. The runtime and
  agent layers keep their existing typed errors (`HarnessError`,
  `ProviderError`); the CLI's `Box<dyn Error>` is the agreed ceiling.

- **Keep task failures inside `SchedulerError::Failed(String)`.** Rejected:
  it reintroduces the ambiguity this ADR removes. Scheduler errors must
  answer "did the scheduler lose this result, and why"; task outcomes are
  already a dedicated, richer channel (`ToolOutput`).

## Consequences

- `loop_guard.rs` is gone; a `git mv` renames it with history intact. Any
  out-of-tree code referencing `muta_agent::loop_guard` breaks loudly at
  compile time — intended (break clean).
- `RunClosure<R>` now returns `Result<R, SchedulerError>`; the scheduler's
  tests assert on the variants instead of string contents.
- `cancel_all`/`abort_all` rejection semantics are unchanged, now
  distinguishable by variant rather than by substring.
- The CLI's three daemon-verb paths compile without adapters; error
  handling in `crates/muta` is now uniform.

## References

- ADR-0148 — doom-guard threshold semantics (the pre-dispatch guard that
  motivated the vocabulary extraction).
- ADR-0113 — the strict first-repeat blocking mode (`threshold = 2`).
- `crates/muta-agent/src/tool_scheduler.rs` — module docs for the two-tier
  cancellation contract this error enum expresses.
- `docs/governance/documentation/core/index.md` — documentation governance.
