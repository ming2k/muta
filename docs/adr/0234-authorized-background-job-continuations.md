# 0234. Authorized background job continuations

- **Status:** Proposed
- **Date:** 2026-09-11
- **Scope:** agent tools, runtime job ownership, session scheduling, model context
- **Deciders:** Project maintainer (pending review)
- **Related RFC:** None; incremental proposal recorded directly as an ADR.

> This is a proposal, not an implemented capability or accepted architectural policy. It proposes a concrete continuation protocol extending ADR-0190 and refining the SystemWake direction in ADR-0212. It preserves ADR-0215's dispatch/control tool separation.

## Context and Problem Statement

A useful asynchronous workflow is: launch work, continue independent work, receive the result, then inspect or act on it without requiring the user to ask again. A background process alone does not implement this workflow. Execution, result retention, model delivery, and authorization to start another round are separate responsibilities.

The current repository exposes two intended background producers, `run_command` and `spawn_agent`, and one controller, `process`. Static inspection found the following implementation gaps; tool descriptions alone must not be treated as proof of support.

| Surface | Observed implementation | Gap |
| :--- | :--- | :--- |
| `run_command(background=true)` | Registers a process job and returns `job_id` through `spawn_process_ex` | Its unconditional automatic-notification promise exceeded the model-delivery implementation — **fixed in Stage 1** (message + detach note now describe `process` collection) |
| `run_command(service=true)` | Separate service path with readiness reporting | **Fixed in Stage 1**: `service` outranks `background`, so the service branch can no longer be pre-empted by the earlier `background` branch |
| Foreground command detachment | Adopts a running process into the background manager | Detachment must not imply authorization for autonomous model calls |
| `spawn_agent(background=true)` | Schema and `explore`/`skill` role validation exist | **Fixed in Stage 1**: the examined execution path still awaited the child round, so the parameter was removed and an explicit request is rejected |
| `process` | Provides `status`, `logs`, `wait`, and `kill` for a `job_id` | The sub-agent wording does not establish that the producer is connected; `wait` currently returns state and log tail rather than a typed child result. `kill` used to signal a settled job's recycled pid — **fixed in Stage 1** |
| Completion mailbox | Receives completion/readiness events | `request_wake_turn` is deliberately a no-op; UI delivery is not model delivery |
| Scheduled command | Tool said the command executes at fire time | **Fixed in Stage 1**: the timer only publishes a digest, so the parameters were removed rather than left as a false promise |

Implementation evidence:

- [Command dispatch](../../crates/muta-agent/src/tools/execute_command/mod.rs): background, timer, and service branches.
- [Sub-agent tool](../../crates/muta-agent/src/subagent_tool.rs): background schema, `is_background` validation, and awaited child execution loop.
- [Job control](../../crates/muta-agent/src/tools/process_jobs.rs): action dispatch and terminal wait output.
- [Background manager](../../crates/muta-runtime/src/background_jobs.rs): process adoption, completion publication, and timer loop.
- [Task mailbox](../../crates/muta-runtime/src/task_mailbox.rs): intentionally disabled wake function.

ADR-0212 prohibits machine outcomes from masquerading as human follow-ups. This proposal retains that boundary rather than restoring the old wake path. It qualifies as an ADR because it crosses runtime/agent/context boundaries and establishes enforceable authorization, ownership, and delivery invariants.

## Decision Drivers

- Complete real background-command and read-only research workflows without adding a second task execution API.
- Make schema, tool results, and actual lifecycle behavior agree.
- Avoid repeated model polling, unsolicited inference costs, and unbounded result injection.
- Preserve human-input precedence, existing execution permissions, and session isolation.
- Deliver a bounded live-runtime feature before considering durable workflow orchestration.

## Considered Options

1. Keep active-round parallelism only; callers explicitly query or wait.
2. Add authorized, event-driven result delivery and continuation to the existing job lifecycle.
3. Add a generic `task(start/...)` tool and a persistent workflow engine.
4. Restore machine-generated human FollowUp requests.

## Decision Outcome

Propose option 2, implemented in stages. Option 1 remains a supported default when continuation is not authorized. This proposal creates no additional task tool and does not rename `process`.

### Producer and controller boundary

- `run_command` starts shell work; foreground/background/service are execution modes, not separate tools.
- `spawn_agent` starts delegated model work. Its background path must transfer child execution to a runtime-owned job before acknowledging success.
- `process` manages existing jobs and retrieves their results. Keep its four actions; extend their structured results only where necessary to represent child outcomes accurately.
- `todo` remains a planning surface, not a runtime scheduler.

The background child path must return `job_id` before child completion, preserve child usage accounting and permission checks, publish a final research result independently of diagnostic logs, and support cancellation through the same job identity. Initially admit only the existing `explore` and `skill` profiles, with their effective read-only restrictions enforced; do not enable background writing agents.

Normalize shell modes before dispatch: `service=true` selects service semantics whether or not `background=true` is also supplied. A scheduled mode combined with immediate background/service execution is invalid and must be rejected before side effects.

**Amends ADR-0190's Timer consumer (landed in Stage 1).** ADR-0190's post-acceptance addendum claimed `run_command { schedule_in_secs, repeat }` as the Timer's model-facing consumer. That pairing was a D7 violation in both directions: the tool told the model "the command runs at fire time" while the runtime only publishes the string as a digest, and with the wake turn disabled (ADR-0212) the digest had no consumer at all. The parameters are therefore removed from the tool surface; `JobSpec::Timer` and the spawn path stay in the fabric, and no tool arms a timer until either a genuine execution contract exists (Stage 1 prerequisite) or SystemWake lands (Stage 2). Re-introducing a scheduler is explicitly not part of this ADR.

**Removes the `spawn_agent` background parameter (landed in Stage 1).** The schema advertised asynchronous dispatch for `explore`/`skill`; the implementation only validated the role and then awaited the child round inside the tool call — no job registration, no `job_id`, nothing to collect. The parameter is gone from the schema and an explicit request is rejected with an actionable error, rather than silently blocking the caller that planned around a later notification.

### Authorization is distinct from background execution

Use one runtime-owned continuation grant associated with the originating user request and its jobs. A grant records scope, revocation state, and a finite continuation budget. Its external configuration syntax remains a review item; model-authored arguments must not create or enlarge the grant.

- Interactive sessions default to no autonomous continuation.
- Headless operation also requires explicit host/operator configuration; being headless is not itself authorization.
- An explicitly authorized interactive workflow may continue, making result admission an explicit user-requested exception consistent with ADR-0212's intent boundary. This extends its headless-focused wake description and requires maintainer approval.
- `background=true`, automatic detachment, and ordinary tool-execution approval do not grant autonomous continuation.
- A grant is not renewed merely because a continuation creates another background job. Descendants share its remaining budget and original scope.
- Revocation or budget exhaustion leaves results inspectable without launching another model call. Reuse existing permission and inference-budget enforcement rather than introducing a second permission system.

The initial recommended wake budget is one additional round per originating request, with further autonomous rounds requiring a larger explicit grant. This is a limit on newly authorized wake rounds, not a change to the general in-round agent loop. Existing host cancellation and resource limits still apply.

### Result delivery and SystemWake admission

Use the existing job manager as the source of execution truth. Add pending-delivery metadata keyed by `(session_id, job_id, event_sequence)`; do not create a parallel task database.

1. Retain the outcome before emitting its notification. Notifications are hints to inspect pending outcomes, not the only copy of a result.
2. With an authorized grant and an active round, admit eligible results at the next safe model-request boundary. Never mutate an in-flight provider request or interrupt a running tool call.
3. With an authorized grant and no active round, schedule a dedicated internal SystemWake driver carrying result identities. Do not insert an `AgentRequest::FollowUp` or fabricate a user message.
4. Dispatch queued human requests before SystemWake. Revalidate session ownership, scope, grant validity, and remaining budget immediately before dispatch; new human intent may invalidate old work.
5. Coalesce results already pending at admission into one continuation. Do not add an arbitrary batching delay or invoke a model once per log line.
6. Claim result identities atomically with request admission and retain an explicit delivery record. If failure occurs before admission, release the claim. After admission, any inference retry uses the same delivery identity and must not enqueue an independent duplicate wake.
7. A terminal result explicitly returned by `process(wait)` is also a delivery path and must suppress a redundant automatic delivery when admitted to model context. State/log inspection alone is not an acknowledgment of the full outcome.

Completion means producer success or failure. Operator cancellation is silent for automatic continuation. Service readiness may be delivered once if authorized; normal continued running does not trigger wakes. Unexpected service death may trigger a separate authorized event. Timers and recurring jobs are excluded from the first delivery protocol.

### Context, cost, and reliability boundaries

The harness supplies identity, event kind, status, exit/error metadata, a bounded summary, and a result reference. Diagnostic logs and sub-agent text are untrusted result data, not system instructions. Context assembly must preserve provenance without forging an unmatched provider tool response. Use the existing request-composition boundary for a dedicated machine-result component.

Set explicit byte/token caps on summaries and batch context. Overflow remains pending or available by result reference; never silently mark an omitted outcome as delivered. Full logs and child results remain retrievable through `process`. A child result must not be represented only by the last few diagnostic lines.

Delivery state applies to the lifetime of the live runtime/session. Reconcile broadcast lag against retained pending outcomes. Bound retention using existing job capacity or an explicit admission limit: completed, undelivered results cannot be silently evicted to admit unlimited new jobs. On session closure, revoke pending wakes and follow the existing job shutdown policy; do not redirect outcomes to another session or resurrect the closed one.

This is duplicate-admission prevention, not exactly-once execution of arbitrary model side effects. Cross-process crash recovery, durable unconsumed-result replay, and external side-effect deduplication are not promised in the first version.

### Invariants & Behavioral Boundaries

- **INV-BG-01 — Truthful capability:** An accepted background request returns a usable `job_id` without awaiting completion. Unavailable background support fails explicitly instead of silently running synchronously.
- **INV-BG-02 — One lifecycle:** Producers share job identity and control; no equivalent `task(start, command)` entry point is added.
- **INV-BG-03 — No implicit authority:** Background execution cannot create or extend permission to run autonomous model rounds.
- **INV-BG-04 — Human precedence:** Machine outcomes never enter the human FollowUp queue; SystemWake neither preempts a round nor outranks queued human requests.
- **INV-BG-05 — Scoped delivery:** A result belongs to its originating session and authorized scope. Duplicate completion signals cannot independently admit duplicate continuations.
- **INV-BG-06 — Bounded cost:** No model-based status polling is required for automatic delivery; summaries, pending storage, and autonomous continuation are bounded.
- **INV-BG-07 — Result provenance:** Child output and process logs remain untrusted data; cancellation, failure, and success retain their distinct meanings.

### Positive Consequences

- AI can overlap independent work and consume results without user reminders when authorized.
- The model learns two producer tools and one controller instead of competing task APIs.
- Passive TaskBar notifications remain inference-free by default.
- Tests can distinguish background execution, UI events, model delivery, and actual continuation.

### Negative Consequences & Trade-offs

- The runtime must track result admission separately from process completion.
- True background children require owned lifetimes, cancellation wiring, and usage accounting instead of borrowing the foreground tool callback.
- Human priority may delay machine continuations indefinitely; this is intentional.
- A one-round default grant may require renewed authorization for longer repair cycles.
- Live-runtime retention does not satisfy workflows that must survive daemon restarts.

## Rejected Alternatives & Negative Knowledge

### Active-round waiting as the only capability

This is simple and remains valid without a grant, but cannot fulfill an explicitly requested “finish later and continue” workflow once the parent round ends. Keeping it as the sole mode would require removing autonomous-continuation claims.

### Another generic task execution tool

A second shell-start API duplicates `run_command`, increases model choice cost, and can diverge in permissions and defaults. Reuse producer-specific schemas and common lifecycle control.

### Machine-generated human FollowUp

Cheap to wire but destroys intent provenance and violates human priority. Do not restore the no-op mailbox by smuggling results into the user request channel.

### Wake on every completion without authorization

Background execution is not permission for unsolicited inference. This causes unnecessary token usage, service-notification storms, and accidental autonomous loops.

### Persistent workflow engine, DAGs, and automatic retries

No demonstrated requirement here needs distributed scheduling, task dependencies, compensation, or crash-safe arbitrary replay. These are deferred until a concrete workflow requires them. The immediate producer and delivery gaps are correctness work, not a justification for general orchestration.

## Implementation Plan and Acceptance

### Stage 1 — Make producer contracts real

Landed in this change (all four items are contract corrections that do not depend on accepting SystemWake):

- `run_command` no longer promises an automatic notification: the background, service, and detach paths state that the job runs independently of the turn and that its outcome is collected with `process` (`wait`/`status`/`logs`). The detached-shell harness note was corrected the same way.
- Mode selection is normalized (`service` outranks `background`) and the misleading scheduled-command surface is gone: `schedule_in_secs`/`repeat` were removed from the tool, with the timer arm's contract documented in `muta-contracts` (see the amendment above).
- `spawn_agent`'s unsupported `background` parameter was removed from the schema and an explicit request now fails with an actionable error instead of silently blocking.
- Job control no longer signals settled jobs: the cancel handle and pid are dropped together at settle, `kill` refuses a job that has no live handle, and a re-arming timer keeps its handle between fires so its loop stays cancellable after the first tick (cancelling one settles its entry silently rather than publishing a digest).

Still open, and the only part that depends on the SystemWake decision:

- Wire true background `explore`/`skill` child ownership — immediate `job_id`, usage accounting, cancellation, and typed final results — into the existing job service. Until then the rejection above is the contract.

### Stage 2 — Add authorized delivery

Both halves have landed. The authorization surface is **provisional** and
flagged for review below.

**Stage 2a — landed.**

- **Retained outcomes replace the dead result channel.** The manager previously
  pushed every settle into an unbounded `mpsc` whose receiver had no consumer
  anywhere in the tree: the only durable copy was the ledger row and the
  transient broadcast. `BackgroundJobManager` now keeps a bounded
  (256-entry) pending delivery queue keyed by a monotonic `sequence`, and
  retention happens *before* the notification is emitted, so a consumer that
  missed (or never saw) the event still finds the result. Eviction is loud and
  leaves the snapshot, ring buffer, and log file readable.
- **Claim is the acknowledgment primitive.** `BackgroundJobService::claim_outcomes`
  returns the not-yet-claimed deliveries for a job and marks them claimed;
  `process(action: 'wait')` calls it and returns the settled summary and log
  path alongside the state and tail. `status`/`logs` deliberately do not claim,
  so inspecting progress is not mistaken for accepting the outcome. Identity is
  per delivery, not per job, so a re-arming timer's later fires are not
  swallowed by deduplication against its first.
- **Claiming governs delivery, not readability.** The complementary accessor
  `BackgroundJobService::settled_result` keeps the job's own most recent
  settlement on its entry, so `status` and a repeated `wait` still report what
  the job produced after the automatic delivery has been spent. Without it, a
  second `wait` would answer `summary: null` — collection would destroy
  readability.
- **`wait` refuses a running service instead of timing out.** A service has no
  terminal state by contract, so blocking on one burned the caller's whole
  budget and ended in a `Timed out` error that conveyed nothing. `wait` now
  returns `service_still_running` immediately and points at
  `status`/`logs`/`kill`; a service that has actually exited has settled and
  waits normally.
- **Broadcast lag no longer kills the notification path.** The per-session
  forwarder in `bootstrap.rs` used `while let Ok(event)`, so one
  `RecvError::Lagged` ended the task and every later task-bar update for the
  session went dark. It now skips the gap and rebuilds rows from the job
  snapshots, matching how the registry and mailbox consumers already treat lag.
- **Ownership survives detachment.** `adopt_process` hard-coded
  `owner_session: None`, so a foreground command detached at the sync budget
  lost the session that detached it — making the later settle undeliverable and
  unrevocable. The session service now stamps the owner.
- **Closing a session drops its pending results.** `kill_session` discards the
  closed session's retained outcomes (daemon-level results are untouched):
  nothing crosses into another session and a closed session is not revived to
  receive one.

**Stage 2b — landed.** `crates/muta-runtime/src/task_continuation.rs` owns the
policy; the driver owns admission.

- **Two decisions, kept pure and tested.** `ContinuationAuthority` reads the
  session's declared posture; `WakeBudget` bounds it. Both are pure so the rules
  are asserted without a live agent, provider, or job fabric.
- **Authorization reuses the unattended posture, and this is the provisional
  choice.** The unattended posture already means "no human is present, never
  wait for confirmation" — exactly the condition under which continuing without
  a prompt is meaningful. Reusing it adds no configuration face that could
  disagree with it. An interactive session is therefore passive *by
  construction*: `ContinuationAuthority::of(false)` denies, and raising the
  budget limit cannot grant authority that was never there. **If a separate
  authorization surface is preferred, `ContinuationAuthority::of` is the single
  place to change.**
- **A wake round cannot re-arm the budget.** The budget is renewed only on
  request-channel activity, and `try_claim` decrements *before* returning, so a
  crash after the claim loses the round instead of repeating it. Default grant:
  one round per originating request.
- **Human precedence is structural, not advisory.** The driver refuses the wake
  outright while any human follow-up is queued, and again while a round is
  running. Refusals leave the result retained and unclaimed, so `process` still
  collects it — nothing is lost by deferring.
- **Vehicle is a dedicated channel.** Wakes ride `SystemWake` on their own
  `mpsc`, never `AgentRequest`, so a machine result cannot be mistaken for human
  intent. The resulting round is driven by a harness-authored digest marked
  `hidden` — the same vehicle the file-mention notes use — and the digest states
  in its first line that it is not a user message.
- **Bounded context.** The digest is capped (`MAX_DIGEST_BYTES`) and batching is
  inherent: one claim per session covers every retained, unclaimed result, so a
  burst of settles produces one continuation, not one per job. Full output
  stays behind `process`.
- **Nothing wakes on readiness or progress.** Only a settled `Completed` outcome
  is offered; a service reporting ready, or a job printing a line, is a task-bar
  fact. An operator `kill` stays silent.

### Stage 3 — Verify the complete journey

| Case | Required observation |
| :--- | :--- |
| Background command | Returns `job_id` while process remains running; `process` reports terminal state and output later |
| Background read-only child | Returns before child completion; parent can do independent work; final research result is retrievable, and usage is attributed |
| Unsupported child or conflicting modes | Explicit rejection before work starts; no synchronous fallback |
| Parent round already ended | Authorized completion starts a SystemWake round, result reaches model context, and AI performs an observable follow-up action |
| Parent round active | Result arrives only at a safe request boundary; no concurrent parent round or provider-request mutation |
| Human input pending | Human request runs first; invalidated or revoked grants do not resume old work |
| Default interactive / no grant | Completion updates task state but generates zero autonomous model requests |
| Duplicate completion / prior terminal wait | No second independent continuation for an already admitted result identity **(2a: `wait` claims the delivery exactly once; a second `wait` reports `deliveries_claimed: 0` while still reporting the summary)** |
| Failure before admission / after admission | Pre-admission claim can be retried; post-admission retry retains identity without a duplicate wake |
| Burst, large output, broadcast lag | Bounded batch context; omitted results remain retrievable/pending; reconciliation does not lose eligible outcomes **(2a: bounded retention with loud eviction; the forwarder rebuilds rows after `Lagged` instead of dying)** |
| Cancellation, service ready/death | Cancellation stays silent; readiness and unexpected death are distinct, deduplicated events |
| Session close / budget exhausted | No wake, cross-session delivery, or grant renewal; retained results follow documented lifecycle limits **(2a: a closed session's retained results are discarded)** |

Use package-level `cargo check` first for implementation changes, then targeted `cargo nextest run` filters for the changed producer, mailbox, scheduler, and context tests. No full package/workspace suites are required for this proposal. An event-only test with “wakes” in its name is insufficient: a deterministic provider fixture must verify result admission and the subsequent model/tool action.

Before release, add a cold-start acceptance journey through the documented public client/API: authorize a bounded workflow, launch background work, let the parent round end, observe completion and a follow-up action without another user prompt. Also demonstrate the default passive case. Publish the authorization interface and lifetime limits alongside the implementation, updating living architecture, tool reference, acceptance documentation, and changelog as applicable. This proposal alone does not change those descriptions of current behavior.

## Review Questions

- **Authorization surface (provisional choice made, please confirm).** The
  implementation reuses the existing unattended posture
  (`ContinuationAuthority::of`) rather than adding a configuration face. If you
  prefer a dedicated `continuation` switch, or a headless-only gate, that one
  function is the single change point.
- **Initial grant.** One wake round per originating request
  (`DEFAULT_WAKE_BUDGET`). Raising it for longer autonomous chains (run tests →
  fix → re-run) is a deliberate, recorded act.
- **Digest and retention limits.** `MAX_DIGEST_BYTES` (4 KiB) and
  `MAX_PENDING_OUTCOMES` (256) are the two bounds; both are constants chosen
  conservatively rather than derived from a measurement.

Review the authorization surface before relying on autonomous continuation.
Stage 1 and the Stage 2a/2b mechanisms do not depend on that answer: with a
passive session the driver never wakes, and explicit `process(wait)` remains the
supported closure.

## Links

- [ADR-0190: Agent-as-actor unified task fabric](0190-agent-as-actor-unified-task-fabric.md)
- [ADR-0212: Follow-up authority and task bar separation](0212-decouple-followup-queue-and-authoritative-task-bar.md)
- [ADR-0213: Model request composition and context lifecycle](0213-model-request-composition-and-context-lifecycle.md)
- [ADR-0215: Tool surface consolidation](0215-tool-surface-consolidation.md)
