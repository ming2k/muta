# 0193. Round-EOL Session Digest: Convergence-Driven Summarization with a Single-Flight Cas Ledger

- **Status:** Accepted
- **Date:** 2026-09-30
- **Builds on:** ADR-0183 (five-phase spatiotemporal aspect engine — this decision wires the fifth phase), ADR-0022 (session-level AI title — the trigger-policy lineage this decision revises), ADR-0186 (single-transcript projection directives — the persistence substrate), ADR-0187 (persistence v2 — `digest_anchor`), ADR-0190 (agent-as-actor — detached task discipline and the contract for unwired harness promises)
- **Supersedes:** the digest-trigger clauses of ADR-0022 §Decision 2 (first-turn-auto and admission-side refresh); the title clauses of ADR-0022 remain in force until this ADR's implementation removes the last admission-side caller

## Context

The session digest — the AI-maintained `{title, intent, history[]}` working-memory
projection rendered in the picker's detail view — is the flagship deliverable of
the Round-EOL phase defined in ADR-0183 §Decision 2.5:

> **Round-EOL Phase (`RoundEolHook`):** Executes upon round convergence to persist
> state, synthesize session titles (first turn), and extract rolling 12-item
> working memory digests.

The implementation diverges from the decision it cites. In
`crates/muta-agent/src/orchestration.rs` (`maybe_refresh_session_digest`,
~lines 1108–1116 and 1589–1624), digest maintenance is **not** a Round-EOL
hook. It fires at **round admission** — immediately after a user prompt is
persisted, before the round's work begins — as a fire-and-forget
`tokio::spawn` that bypasses `AspectEngine` entirely.
`AspectEngine::process_round_eol` (`crates/muta-agent/src/aspects/mod.rs:126`)
exists, is tested, and has **zero callers**; four of the five phases are wired,
and the fifth — the one whose entire purpose is this digest — is the unwired one.
This is precisely the "unwired harness promise" class that ADR-0190 names as a
contract violation.

### Why admission is the wrong moment

Admission-time summarization reads a transcript whose final entry is a **user
prompt no assistant has yet responded to**. Every defect below follows from
summarizing a conversation that has not yet had its next turn.

1. **The survival window is inverted.** The admission trigger places the digest
   probe at the point of *lowest* host survival. A detached task lives inside
   the daemon process; its only true death is process exit. At **round
   convergence** the user has just received the agent's reply and is — by direct
   observation of any interactive session — reading it; the process-alive
   probability is the session's maximum. At **next admission** the user may
   return in five minutes, tomorrow, or never. If the session is never
   resumed, an admission-only digest is permanently stale, missing precisely the
   work that made the session worth summarizing: the round that just closed. The
   task's imperceptibility to the user — the very property cited for keeping it
   off the round path — is the argument **for** the EOL trigger: a costless,
   fail-open probe should run when the host is most likely alive, not as a bet
   that the user will ever return.

2. **The history pollutes forward.** `SessionDigest.history` is documented
   (`crates/muta-agent/src/session_digest.rs:7`) as a working-memory projection
   for the picker's detail view. A digest generated at admission folds an
   unanswered user prompt into the history as though it were settled fact. When
   the next round later diverges — clarification requested, redirect, abandonment
   — the projection records an outcome that never happened, and no later refresh
   is guaranteed to correct it (the anchor may never again cross the refresh
   threshold).

3. **The throttle quantizes to the wrong clock.** The refresh watermark
   (`DIGEST_REFRESH_DELTA_CHARS = 8_000`, `orchestration.rs:1568`) accumulates
   transcript chars since the persisted anchor. Under admission triggering, the
   unit of accumulation is "one full user round plus one dangling prompt." Under
   EOL triggering it is "one completed round." Same constant, but the EOL clock
   yields digests that terminate at a semantic boundary — a converged round —
   rather than at a mid-thought watermark.

4. **The five-phase engine is falsified by its own codebase.** Shipping an
   aspect engine with an orphaned fifth phase while the digest runs around it
   means the architecture document and the call graph disagree. Every future
   contributor reading `aspects/mod.rs` will conclude — wrongly — that
   `process_round_eol` is the digest path.

### What remains correct and is kept

- **Title policy (ADR-0022, as revised by ADR-0186).** A non-`NULL` title is
  terminal: AI-written once from the opening user round, never auto-overwritten,
  manual title wins. First-turn naming from the opening prompt alone is the
  correct latency-optimality point — the opening request alone names the
  session's intent, and ADR-0022's cross-tool precedent (opencode
  `ensureTitle`, claude-code `generateSessionTitle`) locks thereafter. This ADR
  changes **when the digest refreshes**, not the title lock.
- **Fail-open, detached, budgeted.** The digest probe is a zero-tool
  `CognitiveTask` with its own timeout, logging under the system infrastructure
  budget, never blocking the round path (ADR-0183 §Decision 3).
- **Anchor-throttled refresh.** Transcript-delta gating remains the cost
  discipline; only the trigger clock changes.

## Decision

1. **Wire the Round-EOL hook; fire the digest at round convergence.**
   `orchestration.rs` invokes the digest-maintenance path at the round's
   convergence point — after the final assistant message is appended to the
   transcript and before (or concurrent with) the response hand-off — via
   `AspectEngine::process_round_eol` (or the orchestration function it
   delegates to; the aspect engine is the entry contract, the private
   orchestration helper may remain as its implementation). The admission-side
   invocation at `orchestration.rs:1114` is **deleted**, not demoted.

2. **Admission becomes a catch-up checkpoint, not the primary trigger.** A
   session that re-enters with a digest whose anchor lags the transcript —
   because the daemon died between convergence and persistence, or the EOL task
   lost its race with process exit — is repaired at the next admission by the
   same shared routine. Admission checks; convergence drives.

3. **Single flight, enforced by the anchor itself (CAS, no new lock).**
   With two trigger sites sharing one routine, concurrent generation is possible
   (a slow EOL task overlapping a next-round admission catch-up). The anchor
   that already gates refresh is promoted to the concurrency control: before
   persisting, the task re-reads `(digest, anchor)` and persists **only if**
   `transcript_chars > stored_anchor` — the write is a compare-and-set on the
   anchor column. A loser (its snapshot already superseded) discards its result;
   the winner's persist advances the anchor, which in turn disqualifies every
   later redundant probe via the existing
   `digest_refresh_needed` threshold check. No mutex, no task registry, no
   generation counter: the watermark is the lock, and a redundant probe's worst
   case is one wasted single-shot call that fails its CAS and is dropped.

4. **History terminates at convergence.** Because every EOL-fired digest ends
   at a converged round boundary, `SessionDigest.history` never contains a
   prompt awaiting its response. The admission catch-up inherits the same
   property: it reads a transcript whose last entry is a settled assistant
   reply (or nothing new at all).

5. **Title stays on first convergence, unchanged.** The existing rule — title
   derived from the digest and written only while `title IS NULL`, manual titles
   permanent (ADR-0186) — is preserved verbatim. Under EOL triggering, first
   convergence is the first completed round, which is at worst one round later
   than today's admission-side naming; the opening prompt still anchors it.
   Should a first-round crash leave a session untitled, the catch-up at next
   admission titles it.

6. **The aspect-engine entry point is the contract.** The Round-EOL phase's
   entry is `AspectEngine::fire_round_eol` (`aspects/mod.rs`), invoked by
   `orchestration.rs` at convergence via the agent's existing
   `Agent::aspects()` accessor; it delegates to
   `Agent::spawn_eol_digest_maintenance` (the shared CAS routine in
   `session_digest.rs`), so `orchestration.rs` holds no digest pipeline of
   its own. The superseded orphan `process_round_eol(excerpt, previous)`
   — a signature that duplicated `CognitivePipeline::generate_digest`
   without a caller — is deleted: the aspect phase is a lifecycle entry
   point, not a second way to call the Chronicler. A future phase
   (cross-session retrieval, close-time archival summary) extends the EOL
   phase — it does not fork a second trigger path.

## Alternatives considered

- **Keep admission as the sole trigger (status quo).** Rejected: it permanently
  starves the digest of the final round for never-resumed sessions (the most
  common kind), folds unanswered prompts into history, and leaves the fifth
  aspect phase unwired — three defects, one root cause.

- **Both triggers, guarded by a dedicated mutex or single-flight registry.**
  Rejected: a second synchronization primitive duplicates what the anchor
  already expresses. The anchor is per-session, already persisted, and already
  the throttle; making it the CAS preserves single-writer discipline
  (ADR-0163/0187) with zero new state.

- **EOL trigger without admission catch-up.** Rejected: it leaves the
  crash-between-rounds window (EOL task killed by process exit before
  persisting) permanently unrepaired for resumed sessions, and a resumed
  session's first admission is the natural, already-existing repair site.

- **Summarize at compaction time (merge with ADR-0029's compaction).**
  Rejected: compaction is a correctness mechanism (context must fit the model
  window) that runs inline on the round path; the digest is an accounting
  artifact that must stay detached and fail-open. Coupling them makes digest
  freshness hostage to compaction cadence and vice versa.

- **On-close (session end event) full archival summary.** Deferred, not
  rejected: it is a distinct artifact (full-transcript retrieval summary) with
  its own storage and cost model, and ADR-0112 already establishes that client
  disconnect is not session end. This ADR's EOL phase is the natural host for
  it later; nothing here forecloses it.

## Consequences

- **Positive.** The five-phase aspect engine is fully wired (the ADR-0190
  "unwired harness promise" ledger closes its oldest entry); digests terminate
  at semantic boundaries and never record unanswered prompts as history; the
  never-resumed session's digest covers its final round; concurrency control
  reuses existing persisted state with no new locks.
- **Neutral.** Trigger-site count is unchanged in the common case (one probe
  per threshold crossing); the 2.5 s probe budget, fail-open semantics, and
  infrastructure-budget accounting are carried over as-is.
- **Negative.** A round whose convergence coincides with daemon shutdown may
  still lose its final refresh — accepted, because the next admission repairs
  it at O(one call), and the alternative (synchronous persist on the round
  path) would violate the detached-guarantee contract that makes the whole
  design affordable.
- **Migration.** Single change site (`orchestration.rs` trigger relocation +
  CAS guard in the shared routine + `AspectEngine` delegation); no schema
  change (`digest_anchor` from ADR-0187 already carries the CAS state); legacy
  sessions with missing anchors refresh once on first EOL or catch-up, exactly
  as `digest_refresh_needed` already provides. Tests: EOL-fire equivalence,
  CAS loser-discard, catch-up-after-crash, title-lock preservation under the
  new trigger site.

## References

- ADR-0183 — spatiotemporal aspect engine; §Decision 2 phase 5 is the contract
  this ADR finally implements.
- ADR-0022 — session-level AI title; the trigger-policy ancestor whose
  admission-side reflex this ADR revises, and whose title lock it preserves.
- ADR-0186 — single-transcript projection directives; title terminality and
  per-message provenance.
- ADR-0187 — persistence v2; `digest_anchor` as persisted watermark (the CAS
  state).
- ADR-0190 — agent-as-actor; detached task discipline, and the
  unwired-harness-promise ledger this ADR closes an entry in.
- ADR-0163 — unified SQLite event ledger and CAS architecture; the
  compare-and-set precedent.
- `crates/muta-agent/src/orchestration.rs` — `maybe_refresh_session_digest`,
  `digest_refresh_needed`, `DIGEST_REFRESH_DELTA_CHARS`; the relocation site.
- `crates/muta-agent/src/aspects/mod.rs` — `fire_round_eol`; the fifth-phase
  entry point wired by this ADR (the orphan `process_round_eol` it replaces
  was zero-caller dead contract).
- `crates/muta-agent/src/session_digest.rs` — digest normalization and the
  history contract this ADR tightens.
- External prior art (unchanged from ADR-0022): opencode `ensureTitle`
  generate-once; claude-code `generateSessionTitle` one-shot; codex
  `summarize_for_label` fallback.
