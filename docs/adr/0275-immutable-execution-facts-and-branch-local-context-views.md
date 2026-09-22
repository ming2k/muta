# 0275. Immutable Execution Facts and Branch-Local Context Views

- **Status:** Proposed
- **Date:** 2026-09-21
- **Last revised:** 2026-09-22 (proposal contract review)
- **Scope:** `muta-contracts`, `muta-agent`, `muta-persistence`, `muta-runtime`
- **Deciders:** Project maintainers
- **Implementation:** Not implemented by this proposal
- **Evidence baseline:** Working tree at `9cd2b5de`; includes existing uncommitted changes. Source locations below are code spans, not links, so this record does not break when files move.
- **Supersedes / Amends:** See [Relationship to Existing Decisions](#relationship-to-existing-decisions).
- **Sibling decisions:** [ADR-0276](0276-bounded-raw-artifact-capture-and-durable-publication.md), [ADR-0277](0277-unified-context-planner-and-request-compiler.md), [ADR-0278](0278-compaction-as-a-view-commit.md), [ADR-0279](0279-scoped-inspect-retrieval-retention-and-deletion.md), [ADR-0280](0280-versioned-context-policy-and-clean-break-cutover.md)

## Context and Problem Statement

Muta already has SessionIR, context projection, tool-output trimming, causal compaction, and `inspect` retrieval as building blocks, but those capabilities do not yet form a single recoverable, verifiable lifecycle. The risk is not only wasted tokens: compaction rewrites the history topology, compacted views are not fully persisted, several request-construction paths disagree on semantics, folded content has no complete raw stream, and staleness heuristics merge distinct kinds of evidence.

This decision establishes the foundational half of the target architecture: **immutable execution facts + branch-local context views + independent lifecycle axes**. It replaces the current mechanism in which a *context projection* could rebuild or reparent history, and in which validity, model visibility, and storage retention were effectively one state.

This record owns the fact, execution, and branch domain contracts. The sibling records own artifact publication ([ADR-0276](0276-bounded-raw-artifact-capture-and-durable-publication.md)), request admission ([ADR-0277](0277-unified-context-planner-and-request-compiler.md)), checkpoints ([ADR-0278](0278-compaction-as-a-view-commit.md)), retrieval and deletion ([ADR-0279](0279-scoped-inspect-retrieval-retention-and-deletion.md)), and policy/cutover ([ADR-0280](0280-versioned-context-policy-and-clean-break-cutover.md)). Each contract has one authoritative owner; consumers reference it rather than define a second version. These are separately reviewable but dependent decisions, not independently deployable features. Rejecting a prerequisite requires revising its consumers before acceptance; the release dependency and validation gates belong to ADR-0280.

## Decision Drivers

1. Long rounds, cross-round tasks, branching, and restarts must preserve objectives, constraints, unresolved work, and evidence provenance.
2. The model window, process memory, disk artifacts, and provider cache each have a budget; none may stand in for another.
3. Correct behavior must not depend on the model economizing context; types, transactions, and deterministic policy enforce it.
4. Recovery must state exactly what executed, what did not, and what has an unknown outcome; replaying side-effecting tools is not a way to "restore memory".
5. A failure of the canonical write path must be loud; a legacy write succeeding is not evidence that canonical persistence succeeded.

## Current Implementation Evidence

Static call-path review, not a test reproduction or a production incident finding. The two `PARTLY` rows were corrected against the tree during review; they are retained because they document where the current behavior differs from the intent of an existing decision.

| Location / symbol (code span) | Current observation | Required change |
|---|---|---|
| `crates/muta-persistence/src/session/history.rs`, `commit_session_ir` | A failed IR write logs a warning (`tracing::warn!`) and continues legacy `SessionData` synchronization; `session_ir()` has a bridge fallback into `ir_bridge::session_data_to_ir`. | A failed canonical transaction propagates to the caller; a successful legacy write is never evidence of a successful canonical commit. |
| `crates/muta-persistence/src/session/history.rs`, `commit_context_projection` | On a `translate_projection` failure it rebuilds the transcript from the current model window. | Split fact append from view commit; no back door that "rebuilds history from the current window". |
| `crates/muta-persistence/src/session/ir_bridge.rs` | Bidirectional conversion between `SessionData` and SessionIR exists and is reachable from the runtime path. | Deleted from the runtime; format conversion lives only in the offline migration tool. |
| `crates/muta-persistence/src/db/session_ir.rs`, `load_session_ir` | Recovery rebuilds a main timeline and sets `compaction_horizon` to `None`. | Persist branches, view revision, checkpoints, and the recovery cursor in full. |
| `crates/muta-contracts/src/session_ir/types.rs`, `resolve_active_messages`, `linear_path_with_horizon` | Walks the full lineage for the model window, ignoring compaction payloads; differs from the compiler's horizon handling. | Remove as a model-request source; browsing and model projection use explicitly different APIs. |
| `crates/muta-agent/src/compaction/causal_compactor.rs`, `compact_session_ir` | After `append_compaction`, it rewrites the `parent_id` of the preserved tail's **head node** (`first_kept.parent_id = Some(compaction_node_id)`). | The checkpoint references a source interval; the branch switches views; no fact edge is rewritten. |
| `crates/muta-agent/src/compaction/file_tracker.rs`, `FileOperations::extract_from_message` | Derives reads/edits from assistant-proposed tool calls (`Role::Assistant` + `tool_calls`), with no correlation to `tool_call_id` results or success. | Record attempted / confirmed / failed / unknown separately; only a confirmed effect is a modification fact. |
| `crates/muta-agent/src/round_lifecycle.rs`, `RoundLifecycle` | Isolates a round with `generation: AtomicU64`, a cancellation token, and `ParkedInterrupt { reason }`. | Keep the semantics; persist terminal states together with context-lifecycle events. |

## Considered Options

| Option | Verdict |
|---|---|
| Keep transcript and IR as dual tracks with a long-lived bridge | Rejected: recovery, branching, deletion, and retrieval would each need two sets of semantics. |
| Let context projection double as the fact writer (rebuild transcript from the window) | Rejected: makes the live window authoritative and loses facts on projection faults. |
| Keep one "staleness/retention/visibility" state per message | Rejected: validity, representation, and retention are independent and cannot share a lifecycle. |
| Immutable facts + branch-local views + orthogonal lifecycle axes | Recommended: clear boundaries, verifiable invariants, one fact authority. |

## Decision Outcome

The complete domain model below is recommended for adoption. MUST language states a constraint that binds once this proposal is accepted; it is not a claim about current implementation completeness.

### 1. Scope model: session, branch, task, round, turn, attempt

```text
Session
  └─ Branch: immutable ancestry + mutable revisioned cursor/view
       ├─ TaskScope: objective, requirements, unresolved work
       │    └─ Round 1 ... Round N
       │         └─ Turn 1 ... Turn M
       │              ├─ Provider Attempt 1 ... K
       │              └─ Committed execution group / tool results
       └─ Other TaskScopes and explicitly imported evidence
```

- **Session** is the container for history, artifact authorization, and recovery, not an unbounded message array injected into the model.
- **Branch** isolates the execution path and the context view. A fork shares immutable facts; a branch's invalidation, summaries, pins, and task state do not propagate implicitly to siblings.
- **TaskScope** represents a cross-round user objective. It does not create another execution loop or pursue work automatically. Each round has one primary task and may reference others; a user correction appends a requirement revision instead of overwriting the original. When attribution cannot be established, the association stays uncertain and no model guess closes the task.
- **Round** is the execution-responsibility boundary of one admitted request. Completion, cancellation, supersession, and error all have terminal states; a completed round is not a completed task.
- **Turn** is one accepted model output plus its tool execution group. A turn with no tool output may end the round; concurrently executing tools still share one execution group.
- **Attempt** is a provider attempt inside a turn. An uncommitted stream never enters fact history and may exist as a short-lived diagnostic artifact. A retry reuses the same snapshot only while its authorization and source dependencies remain valid; every dispatch is reauthorized under [ADR-0279](0279-scoped-inspect-retrieval-retention-and-deletion.md#7-revocation-and-in-flight-work). Re-projecting is a new request revision.

Internal IDs are independent of provider tool-call IDs; the latter only serve wire mapping. Every fact, artifact, and retrieval handle carries its full scope. The same provider call ID appearing in different turns cannot cross-read.

### 2. Canonical domain model

The structures below are the target domain shape, not a statement about the current Rust API. Every field must have serialization, versioning, and recovery tests.

| Object | Required fields / relations |
|---|---|
| `FactNode` | `id`, `session_id`, `branch_origin`, `parent_ids`, `seq`, `round_id`, `turn_id`, `payload`, `source_authority`, `sensitivity`, `artifact_refs` |
| `BranchState` | `branch_id`, `head_fact_id`, `revision`, `active_task_id`, `active_view_id`, `execution_cursor` |
| `TaskState` | User-sourced objective / requirement revisions, open items, dependencies, confirmed milestones; state changes carry provenance |
| `Observation` | Source execution ID, resource version, range, exit status, capture completeness, raw artifact, deterministic preview |
| `ExecutionRecord` | Call intent, authorization result, dispatch state, result-acceptance state, external execution state, effect knowledge, ownership generation, confirmed effects and reconciliation references |
| `ContextView` | `view_id`, `branch_id`, `basis_revision`, `checkpoint_id`, `tail_after_fact_id`, `representations`, `policy_revision` |
| `ArtifactManifest` | Scoped ID, content hash / chunk manifest, media type, stream information, completeness, byte count, retention policy |
| `RequestManifest` | Request/attempt IDs, fact/view/policy and authorization revisions, source dependencies, model/tool/instruction and compiler/lowering versions, ordered block hashes, budget provenance, decision reasons |

SessionIR is the domain aggregate view over these objects; the fact, task, and artifact tables are not competing sources of truth. Large payloads need not be loaded with the whole SessionIR; an `ArtifactRef` reads them on demand. Derived indexes and token caches may be dropped and rebuilt.

`source_authority` distinguishes at least user requirements, project instructions, tool observations, assistant inference, and derived summaries. A summary must not gain higher authority merely because it is placed inside a checkpoint; provider lowering preserves untrusted-content boundaries.

A requirement entry extracted from natural language references an exact source span; model-generated interpretation is labeled a *derived candidate*. When a message cannot be split reliably, the original message remains the active requirement, and an inferred "user intent" is never promoted to new authority automatically. Task completion may record an assistant report, but a verified milestone must additionally reference the corresponding execution evidence.

### 3. Orthogonal lifecycle axes

```text
Validity:       Current | NeedsRevalidation | Superseded(by) | Unknown
Representation: Full | Excerpt | Summary | Reference | Omitted
Retention:      Ephemeral | SessionBound | RetainedUntil(deadline) | Pinned
Deletion:       Present | DeletePending | Purged
Capture:        Complete | Interrupted | Truncated(reason) | Unavailable(reason)
```

Three axes are primary and independent (validity, representation, retention); deletion and capture are two additional orthogonal states that must never be folded into them.

Validity is evaluated relative to a branch, resource versions, and the purpose of the query: an older fact remains true, it simply no longer proves the behavior of the current version. A validity event never rewrites the original fact.

Representation belongs to a `ContextView`, not to the fact as a mutable attribute. Reducing representation fidelity changes neither the raw material nor its retention; restoring the raw material produces a new view/request and does not "undo historical compaction".

Deletion and capture are additional orthogonal states: `Purged` must not be written as "temporarily outside the model window", and an interrupted command cannot promise a complete log. An expired authorization attestation is likewise not equivalent to expired content.

### 4. Lifecycle transitions

| Trigger | Must happen | Must not happen |
|---|---|---|
| Round admission | Commit the user input, task association, round identity, and running cursor | Create an execution round when admission was refused |
| Request preparation | Pin dependencies, plan and compile a candidate, validate lowering, then atomically commit the view and request manifest ([ADR-0277](0277-unified-context-planner-and-request-compiler.md)) | Trim history downstream or make a failed candidate the active view |
| Provider completion | Commit the final assistant output and call intents; establish the execution group | Run the carried tools before the stream completed |
| Tool dispatch | Persist dispatch state before the external action | Treat an unacknowledged action as not having happened after a crash |
| Tool observation | Capture raw output; commit result/effects/completeness; evaluate dependent validity | Infer "the modification succeeded" from tool-call arguments |
| Turn boundary | A complete group may be folded; an open group keeps the wire-mandatory parts | Split a call and its unfinished result onto different compaction sides ([ADR-0278](0278-compaction-as-a-view-commit.md)) |
| Round completed | Release execution ownership, free round-scoped temporary blocks, adjust image leases | Automatically mark every task and unresolved item complete |
| Cancel / supersede / error | Commit the reason, partial results, and unknown execution state; the old generation no longer advances the new cursor | Substitute natural completion for an interruption, or re-run an executed tool |
| Task completed | Preserve the user requirement and the result index; large observations may leave the working set | Treat one model statement of "done" as an independent verification pass |
| Branch switch / resume | Restore that branch's cursor, view, task, and execution state | Apply another branch's horizon or a global stale marker |
| Session close / retention tick | Run a bounded batch of indexing/collection and save the continuation cursor | Start an unbounded background task inside the agent loop |

### 5. Two lifecycle operations and their bounds

| Operation | Applies to | Result |
|---|---|---|
| `DropEphemeral` | Reconstructible environment hints, duplicate progress state, uncommitted stream fragments | Releases the temporary payload; keeps only a restricted attempt artifact when diagnostics are needed |
| `Invalidate` | Observations whose dependency version changed | Marks as needing revalidation; does not claim the problem is resolved |

Direct erasure applies only to data declared ephemeral and discardable, or to an explicit purge ([ADR-0279](0279-scoped-inspect-retrieval-retention-and-deletion.md)). Confirmed tool side effects, user requirements, and unresolved work must not travel through `DropEphemeral`.

### 6. Evidence validity and causal replacement

Validity must rest on typed evidence; natural-language keywords and tool names are never a strong basis for deletion.

| Observation kind | Identity / version | Invalidation or replacement rule |
|---|---|---|
| File read | Workspace ID, normalized resource ID, content hash, read range | Only the same version with full coverage may be deduplicated; a version change marks revalidation; different pages are complementary |
| Search result | Query, root directory, filters, resource snapshot / epoch | A new query does not automatically replace an older one; changes to relevant resources make the conclusion stale |
| Build / test | cwd, argv or script identity, test selection, relevant environment summary, input version, exit status | Only a new execution with equivalent scope and conditions may be marked superseded; different packages or test sets keep their own evidence |
| File modification | Committed execution result, before/after hash, actual effects | Failed or refused calls do not count as modifications; partial success records the concrete effects |
| Image | Original image hash, source resource / render version, visual task reference | A source change invalidates "the current screenshot"; a comparison task keeps the older image as historical evidence |
| External information | Source, acquisition time, explicit freshness policy | Expiry means needs verification; TTL is not used to infer that a fact became false |
| User requirement | User message ID, scope, subsequent revision relations | Only the appropriate authority may revise it; an assistant summary cannot cancel a requirement |

An unparseable shell command yields an opaque command identity; when dependencies cannot be established, the workspace epoch produces a conservative `NeedsRevalidation` rather than a claim that another command covered it. External file changes are captured by a watcher plus hash verification at read and commit time; Git HEAD alone does not describe a dirty worktree.

A resource change in one workspace may affect several branches, but impact is assessed per branch against that branch's version references. Model-cache stability must not block timely invalidation: prefer appending an explicit invalidation block at the tail, and recompile older content when necessary.

### 7. Persistence, crash recovery, and execution safety

The recommended final schema family is `sessions`, `branches`, `facts`, `execution_records`, `task_revisions`, `context_views`, `checkpoints`, `artifact_manifests`, `artifact_refs`, `request_manifests`, `deletion_jobs`. The concrete table split may change, but the transaction boundaries below are not negotiable.

| Transaction | Atomic content |
|---|---|
| Admission commit | User fact, round/task identity, branch head/revision |
| Assistant commit | Completed model output, tool intents, execution cursor, request association |
| Dispatch commit | Tool execution ID, authorization result, dispatch state |
| Result commit | Result fact, published artifact references, confirmed effects, execution state, branch revision |
| Projection/request commit | Checkpoint (if any), validated view and request manifest, reference relations, branch active view / revision and cursor; verify source and authorization revisions |
| Deletion commit | Tombstone, access revocation, dependent invalidation, resumable collection job |

Session writes serialize through the existing single writer; a transaction validates the expected revision and an idempotent operation ID. A committed UI event or a releasable execution state is published only after the commit succeeds. Streaming display may happen earlier but must be marked provisional and delivered under the revocation rules of ADR-0279.

A database transaction cannot give a shell command or an external service exactly-once semantics. When a crash happens after dispatch but before the result commit, recovery reports `OutcomeUnknown`; the query may verify the state or demand an explicit retry. Automatic call recovery is allowed only when the tool declares and verifies an idempotency key or a resumable protocol. The system must never re-modify a file or re-invoke a remote action just to rebuild a lost observation.

A request manifest pins the model, tools, instructions, fact versions, temporary-block hashes, and media references. When a temporary block must be reproducible it is stored as a request artifact under a retention policy and does not enter conversational facts. The manifest contains no authentication headers. Once a raw artifact is purged, the manifest explicitly supports structural audit only and cannot claim a full replay.

Restart restores the cursor, view, checkpoint, and tasks per branch, and cross-checks artifacts against open execution records. An unexpectedly missing reference or an incompatible version returns a typed recovery error; an authorized tombstone instead restores the explicit deleted/unavailable state under ADR-0279; execution does not continue through `ir_bridge` or by treating the current message array as history.

### 8. Execution closure and late evidence

Result acceptance, external execution, and knowledge of effects are separate dimensions:

| Dimension | Meaning |
|---|---|
| Result acceptance: `Open` / `Closed` | Whether an ordinary result may still be committed into the original execution group |
| External execution: `NotDispatched` / `InFlight` / `TerminationConfirmed` / `OutcomeUnknown` | What is known about the external action; cancellation of waiting is not termination evidence |
| Effect knowledge: `Unverified` / `Partial` / `Confirmed` | Evidence about actual effects, independent of exit status and capture completeness |

Closing result acceptance is a writer transaction. It records a result or an explicit interruption disposition for every outstanding call and fences the former execution owner's generation. A racing result either commits before closure, or is handled as late evidence after closure; it cannot disappear between the two paths. Closure never converts `InFlight` or `OutcomeUnknown` into confirmed termination. The protocol representation of an interrupted call states what is unknown; it does not fabricate a successful result.

A late result is appended once as a reconciliation fact, carrying its internal execution ID, original scope, producer identity, and deduplication key. It never reopens or rewrites the old group, advances a newer round's execution cursor, or initiates autonomous execution. Conflicting evidence is retained as a conflict, not silently overwritten. It updates derived execution knowledge and invalidates affected views/checkpoints through their source dependencies; an immutable old request remains a record of what was sent. Branches evaluate the new evidence against their own dependencies; a correction does not silently change sibling task state.

A closed group may be compacted while an external outcome remains unknown, but that unresolved outcome is mandatory task/evidence state until verified or explicitly dismissed by an authorized decision. Compaction cannot make it eligible for automatic replay. This rule governs observed late events and bounded reconciliation; it does not authorize background command adoption. Deletion and revoked-scope handling of late payloads follow [ADR-0279](0279-scoped-inspect-retrieval-retention-and-deletion.md#7-revocation-and-in-flight-work).

### 9. Payload classification

Every fact payload and derived record carries a sensitivity class. Existing secret-scrub rules apply to the model-visible preview; a raw artifact inherits the correspondingly tighter access policy and cannot be reached around it through `inspect` ([ADR-0279](0279-scoped-inspect-retrieval-retention-and-deletion.md)). Raw secrets must never be used as log, telemetry, or request-manifest fields, and a purge must cover the same content everywhere it was derived.

## Invariants & Behavioral Boundaries

- **`[INV-FACT-01]` Single Fact Authority**: Only canonical SessionIR persistence commits facts; there is no dual write and no fallback authority. (Verification: dependency and call-path inspection, fault injection.)
- **`[INV-FACT-02]` Non-Destructive Projection**: Ordinary projection and compaction never modify a committed fact payload or its ancestry. (Verification: compare fact hashes and parent edges before and after compaction.)
- **`[INV-FACT-03]` Atomic View Commit**: Branch view, checkpoint, and cursor commit atomically and recover completely. (Verification: crash/reopen at every transaction boundary.)
- **`[INV-FACT-04]` Orthogonal Lifecycle Axes**: Validity, representation, retention, deletion, and capture never convert into one another implicitly; `Purged` is never reported as "out of the window". (Verification: lifecycle state-transition tests.)
- **`[INV-FACT-05]` Authority Preservation**: Derived summaries and checkpoints never raise source authority, and an attempted effect is never recorded as a success. (Verification: malicious tool text and refused/failed tool fixtures.)
- **`[INV-FACT-06]` No Side-Effect Replay**: A retry never replays a confirmed or unknown side effect; an unacknowledged dispatch recovers as `OutcomeUnknown`. (Verification: dispatch/result crash matrix.)
- **`[INV-FACT-07]` Payload Classification**: Every fact payload and derived record carries a sensitivity class; secrets never enter fact, log, telemetry, or manifest fields, and a purge covers every derived copy. (Verification: secret-scrub fixtures and inspect-bypass attempts.)
- **`[INV-FACT-08]` Closure Is Not Termination**: Closing result acceptance fences the original execution owner without asserting external termination or known effects; late evidence is deduplicated, appended as reconciliation, and invalidates dependent derived state without reopening the group or advancing another round. (Verification: result-versus-cancel races, late success after timeout, duplicate/conflicting events, and restart with unknown effects.)

## Positive Consequences

Context reduction becomes explainable and retrievable, long rounds continue safely, and branching/restart no longer depend on implicit window state. Tools, images, summaries, and cross-session retrieval share one artifact and lifecycle vocabulary. Once the compatibility paths are deleted, a new capability requires only one fact commit and one projection rule.

## Negative Consequences & Trade-offs

Keeping complete raw text raises disk and I/O cost; chunking, deduplication, explicit retention, and quotas control it ([ADR-0276](0276-bounded-raw-artifact-capture-and-durable-publication.md)), and silent discard is not the answer. Transaction and capture ordering adds latency, so batched writes need measurement and optimization, but acknowledged durability must never degrade to best-effort.

Source manifests, task revisions, and dependency versions add schema complexity in exchange for verifiable recovery and invalidation semantics. A one-time cutover breaks compatibility with older clients and databases; offline migration plus explicit version rejection replaces long-lived compatibility code ([ADR-0280](0280-versioned-context-policy-and-clean-break-cutover.md)).

## Relationship to Existing Decisions

While this record is `Proposed`, existing Accepted ADRs are not rewritten. On acceptance, the new decisions must explicitly address the relationships below.

| Existing decision | Disposition after acceptance |
|---|---|
| [0019](0019-model-relative-context-compaction.md), [0021](0021-pruning-is-implicit-and-distinct-from-compaction.md), [0023](0023-relevance-aware-tiered-pruning-and-layered-token-accounting.md) | Keep model-relative budgets and the lightweight/summarizing distinction; replace multi-entry policy, heuristic invalidation, and legacy storage mechanisms |
| [0040](0040-session-state-and-context-projection.md), [0048](0048-session-as-single-source-of-truth.md) | Keep the projection/fact separation intent; replace the legacy representation with the new canonical schema and single writer |
| [0120](0120-tokens-first-class-unit.md) | Preserve token-first accounting; fold the versioned budget policy into [ADR-0277](0277-unified-context-planner-and-request-compiler.md)/[ADR-0280](0280-versioned-context-policy-and-clean-break-cutover.md) |
| [0186](0186-single-transcript-projection-directives-persistence.md) | Retire the remaining transcript-as-authority persistence; the transcript is a projection, not a fact source |
| [0237](0237-one-structural-query-tool-and-versioned-mutations.md) | Reuse versioned mutations and conflict semantics for view and fact commits |
| [0187](0187-persistence-v2-incremental-append-and-blob-reference-ledger.md) | Reuse the blob reference ledger and honest compatibility stance; the ledger becomes the artifact reference store |
| [0196](0196-supervised-persistence-writer-and-typed-persistence-errors.md), [0231](0231-one-door-to-the-database.md) | Preserve the single supervised writer and one-door rule; all new commits go through it |
| [0217](0217-request-components-and-derived-cache-plan.md) | Preserve component/wire-cache planning; the cache plan is consumed by the compiler in [ADR-0277](0277-unified-context-planner-and-request-compiler.md) |
| [0218](0218-durable-request-projection-archive.md) | Preserve the durable request-projection archive; it becomes the request-manifest/request-artifact store |
| [0056](0056-model-context-assembly-boundary.md), [0061](0061-atomic-model-request-boundary.md), [0213](0213-model-request-composition-and-context-lifecycle.md) | Converge on the single planner/compiler; keep the request snapshot and temporary-context boundary |
| [0236](0236-durable-turn-commits-and-recoverable-projections.md), [0241](0241-session-ir-causal-graph-and-compiler-pipeline.md), [0249](0249-session-ir-native-execution-runtime-and-durable-suspension.md), [0251](0251-causal-dag-branching-timelines-and-unified-aside-multiverse.md) | Strengthen fact immutability, complete branch/view recovery, and unknown-side-effect semantics; policy moves to the agent |
| [0254](0254-epistemic-observation-lifecycle-ingestion-folding-and-near-tail-invalidation.md) | Replace the fixed near-tail/deep-water assumptions and unconditional image eviction; unify version dependencies, leases, and raw capture |
| [0255](0255-session-ir-native-causal-compaction-and-legacy-dual-track-eradication.md) | Replace the reparent/horizon scheme; keep native IR, structured summaries, and the dual-track removal goal |
| [0261](0261-purge-of-nanny-prompting-and-attention-frugal-context-architecture.md) | Preserve attention-frugal assembly and physical runtime enforcement; the system-prompt policy is orthogonal and unchanged |
| [0262](0262-demand-paged-epistemic-memory-and-unified-inspect-channel.md), [0264](0264-epistemic-stream-spooling-content-aware-projection-and-unified-inspect-architecture.md) | Converge artifact capture, authorized handles, completeness, paging, and GC; replace the unbounded "permanently lossless" promise |
| [0263](0263-axiom-of-linear-causality-and-pure-execution-primitives.md) | Preserve finite single-shot execution and zero background adoption; the capture bounds in [ADR-0276](0276-bounded-raw-artifact-capture-and-durable-publication.md) adopt its finite-execution contract |

Sequencing: this record does not depend on [0263](0263-axiom-of-linear-causality-and-pure-execution-primitives.md) being accepted; where both are accepted, the finite-execution contract of [0263](0263-axiom-of-linear-causality-and-pure-execution-primitives.md) governs command lifetime, and this record governs what is persisted about it.

## Rejected Alternatives & Negative Knowledge

### Mutating the causal graph to insert summaries

Implementation is locally simpler, but it changes the parent edges of executions that already happened, widening branch-sharing and recovery risk. A summary is a derived representation of history, not an execution that happened; a view manifest expresses that directly.

### Treating cancellation as proof of no effect

Cancelling a waiter cannot establish whether an external service or process acted. Keep acceptance closure, external execution, and effect knowledge separate; append late evidence and never recover by replaying an unknown action.

### Making the live model window the fact writer

Rebuilding the transcript from the current window makes projection faults lossy and makes the window authoritative. Facts commit on their own path, independent of what the model currently sees.

### One TTL, or a single staleness flag, for all context

Time cannot express file versions, user constraints, or unfinished tasks, and validity is not retention. Each axis keeps its own typed state.

## Links

- Sibling decisions: [ADR-0276](0276-bounded-raw-artifact-capture-and-durable-publication.md), [ADR-0277](0277-unified-context-planner-and-request-compiler.md), [ADR-0278](0278-compaction-as-a-view-commit.md), [ADR-0279](0279-scoped-inspect-retrieval-retention-and-deletion.md), [ADR-0280](0280-versioned-context-policy-and-clean-break-cutover.md)
- Target blueprint (updated when this proposal is implemented): [`docs/architecture/session-ir.md`](../architecture/session-ir.md)
- External references: [Atomic Commit In SQLite](https://www.sqlite.org/atomiccommit.html)
