# 0278. Compaction as a View Commit

- **Status:** Proposed
- **Date:** 2026-09-21
- **Last revised:** 2026-09-22 (proposal contract review)
- **Scope:** `muta-agent` (compaction, checkpoint builder), `muta-contracts` (view/checkpoint types), `muta-persistence` (projection commit)
- **Deciders:** Project maintainers
- **Implementation:** Not implemented by this proposal
- **Evidence baseline:** Working tree at `9cd2b5de`. Source locations are code spans, not links.
- **Builds on:** [ADR-0275](0275-immutable-execution-facts-and-branch-local-context-views.md) (immutable facts, branch views)
- **Amends:** the causal-compactor reparent/horizon scheme and the `resolve_active_messages` lineage walk
- **Sibling decisions:** [ADR-0276](0276-bounded-raw-artifact-capture-and-durable-publication.md), [ADR-0277](0277-unified-context-planner-and-request-compiler.md), [ADR-0279](0279-scoped-inspect-retrieval-retention-and-deletion.md), [ADR-0280](0280-versioned-context-policy-and-clean-break-cutover.md)

## Context and Problem Statement

The current compactor inserts a checkpoint node and then rewrites history: it reparents the preserved tail's head node under the new checkpoint (`first_kept.parent_id = Some(compaction_node_id)`). That mutates the ancestry of executions that already happened, which widens branch-sharing and recovery risk and makes retrieval depend on a mutated parent chain. Compaction also produces summaries with no explicit bound on input or output here, and the split point is chosen by a heuristic over message roles.

This decision separates the two concerns: **facts never change; only a branch's context view changes.** A checkpoint is a derived record that *references* a fact interval and the previous checkpoint; the branch switches to a view that starts from the checkpoint and replays only the tail after a real ancestry boundary.

## Current Implementation Evidence

| Location / symbol (code span) | Current observation | Required change |
|---|---|---|
| `crates/muta-agent/src/compaction/causal_compactor.rs`, `compact_session_ir` | After `append_compaction`, rewrites the preserved tail's **head node** `parent_id` to the checkpoint node. | The checkpoint references a source interval; the branch switches views; no fact edge is rewritten. |
| `crates/muta-contracts/src/session_ir/types.rs`, `linear_path_with_horizon`, `resolve_active_messages` | The model window stops at a `NodeKind::Compaction` node and skips non-`Message` payloads; a second horizon path differs. | One view model; the checkpoint carries `tail_after_fact_id` and a source manifest. |
| `crates/muta-agent/src/compaction/split_compaction.rs` | The cut loop rewinds **away from / past** any `Role::Tool` node (`while cut_index > 0 { if role == Tool { cut_index -= 1; continue } ... }`), so the chosen split point is never a tool result; the assistant turn is the split unit. | Keep the assistant/tool-pairing rule but prove it by execution-group closure, not by role inspection alone. |

Review note: an earlier draft of this decision described the split loop as "rewinding to a `Role::Tool` message". That was inverted; the retained observation above is the corrected one. The intent — never split between a tool call and its result — is correct and is what the closure rule formalizes.

## Decision Drivers

1. Compaction must be reversible at the model layer and invisible at the history layer: authorized retrieval must retain access to original facts within their explicit retention/deletion policy.
2. A compaction must never split an execution group; pending result acceptance and external outcomes must remain explicit even after cancellation.
3. Summaries consume tokens; their input and output must be explicitly bounded, and failures must degrade deterministically without losing mandatory state.
4. Checkpoint construction must not hold a session lock or race a new commit onto the same branch.

## Considered Options

| Option | Verdict |
|---|---|
| Reparent the tail under a synthetic checkpoint node | Rejected: mutates ancestry of past executions and complicates branch sharing. |
| Keep a global `compaction_horizon` per session | Rejected: horizon is a branch/view property, not session-global. |
| Checkpoint as a derived view record with a source manifest | Recommended: facts and edges untouched; retrieval unaffected; branches independent. |

## Decision Outcome

### 1. Model

```text
Immutable facts:       A -> B -> C -> D -> E -> F
Checkpoint K:          source_manifest = [A, B, C, D]
Branch view V2:        checkpoint = K; tail_after = D; live head = F
Model representation:  K + E + F
History / inspect:     A, B, C, D, E, F remain unchanged
Sibling branch view:  retains its own checkpoint and cursor
```

A `Checkpoint` is a derived context record that references a fact interval and the previous checkpoint; it is never inserted as a new execution parent, and no existing edge is reparented. `tail_after_fact_id` points at the actual ancestry boundary, so reading the tail needs no scan of the whole older history. For wide spans the source manifest may use a hash-verified ordered interval index.

### 2. Execution-group closure, and what "closed" means

The call intents, committed results or interruption dispositions, accompanying images, and mandatory protocol blocks form one execution group. A compaction boundary never passes through a group. The compacted interval contains only groups whose result acceptance is closed; a wholly preserved open tail group does not prevent compacting earlier closed groups. The location of a `Role::Tool` message alone proves neither closure nor a legal boundary.

Closure follows [ADR-0275 §8](0275-immutable-execution-facts-and-branch-local-context-views.md#8-execution-closure-and-late-evidence): every call has a committed result or an explicit interruption disposition, and the writer fences further ordinary results into the original group. External termination and side effects may remain unknown. Preserve those unresolved outcomes as mandatory state even when their original group moves into a checkpoint; closure never authorizes replay.

A late reconciliation fact remains outside the original group. A candidate checkpoint that predates relevant reconciliation fails its dependency check. An existing checkpoint remains historical, but a dependent active view must carry the correction or be rebuilt before another request is admitted. No fact edge, old result, or historical request is rewritten. Splitting after a closed turn inside an unfinished round is allowed while the user intent, task state, and evidence needed to continue remain protected.

### 3. Summary structure

The summary contains at least:

- the objective and the current phase, referencing the original user requirement;
- exact effective constraint entries with source references; an excerpt is allowed only when it retains the entire active constraint, not merely an ID or a shortened interpretation;
- confirmed completed work, attempted/failed work, and unresolved external outcomes in separate fields;
- unresolved items, blockers, and the next verification step;
- key findings with evidence IDs and resource versions;
- file effects computed from execution results;
- the prior checkpoint, source scope, omission classes, and generation version.

Mandatory task state is assembled deterministically from authorized requirement revisions and execution evidence, outside the model-generated narrative. Finding an evidence ID or a required heading in a summary is not proof that its requirement was preserved. The compiler includes the actual mandatory content and meters it, while schema/source checks validate the derived narrative separately. A retained reference permits retrieval but does not substitute for an active constraint. When a constraint entry alone exceeds the budget, admission failure applies ([ADR-0277](0277-unified-context-planner-and-request-compiler.md)); unlimited lossless semantic compression is not promised. Low-priority historical user text may retain only an index; active requirements are never guessed back out of a rolling summary.

### 4. Generation and commit

Bound the summary input and shard it per source for map/reduce. Every model call uses the non-recursive `CheckpointSummary` admission intent of [ADR-0277 §6](0277-unified-context-planner-and-request-compiler.md#6-non-recursive-internal-requests), with that record owning recursion and accounting rules. Every output has an explicit generation-token ceiling, a 45-second per-call timeout, and a shared total build budget for tokens, calls, reduction depth, and monotonic elapsed time. Cancellation and deadlines must interrupt pending calls; checking elapsed time only after an unbounded call returns is insufficient. Validate schema, reference existence, mandatory-entry coverage, state enumerations, and output size first; after a failure allow at most one bounded repair, then fall back to a deterministic template. The fallback preserves mandatory state and references the remaining evidence, never nests a full older summary recursively, and never does arbitrary byte slicing. For UTF-8 boundary requirements see [Rust `str` slicing](https://doc.rust-lang.org/std/primitive.str.html#impl-Index%3CRange%3Cusize%3E%3E-for-str).

Structural validation cannot prove that an LLM summary is semantically correct. Deterministic logic owns critical constraints, execution effects, and source coverage; the remaining summaries carry a derived marker, with retrieval and regression evaluation controlling the risk.

Generation runs outside the transaction; at most one checkpoint build per branch is in flight. The final view/request commit defined in [ADR-0277](0277-unified-context-planner-and-request-compiler.md) compare-and-swaps the pinned head/task/policy revisions and verifies source/capability/authorization dependencies, including reconciliation and deletion changes. Checkpoint construction alone never changes the active view. New input or a branch change discards the candidate or triggers re-planning, and a stale summary never overwrites newer state. A cancellation leaves no half-applied view. The number of CAS attempts per round is bounded (the policy default is 3); on exhaustion the candidate is discarded and the caller receives a typed error rather than retrying indefinitely (the finite-planning bound of [ADR-0277](0277-unified-context-planner-and-request-compiler.md) supplies the general rule).

## Invariants & Behavioral Boundaries

- **`[INV-CKPT-01]` Execution-Group Closure**: No group is split; compacted groups have fenced result acceptance under ADR-0275 §8. A preserved open tail is legal. Closed groups with unknown external outcomes retain mandatory unresolved state, and late evidence invalidates dependent projections without reopening history. (Verification: concurrent tools, open-tail compaction, late results after cancellation, and unknown outcomes across restart.)
- **`[INV-CKPT-02]` Source-Referential Checkpoint**: A checkpoint references a fact interval (source manifest) and the prior checkpoint; it is never inserted as an execution parent and no existing fact edge is reparented; `tail_after_fact_id` points at the real ancestry boundary. (Verification: fact hash and parent-edge comparison before/after compaction; sibling-branch invariance.)
- **`[INV-CKPT-03]` Mandatory-State Preservation**: A summary preserves active user requirements (with source references), effective constraints in full, unresolved items and external outcomes, confirmed effects, and evidence IDs; repeated summarization must not silently drop them, and a fallback preserves mandatory state without recursive growth. (Verification: ≥10 consecutive summarizations and model-switch fixtures.)
- **`[INV-CKPT-04]` Bounded, CAS-Guarded Build**: Generation runs outside the transaction with per-call and total time/token bounds; at most one in-flight build per branch; the validated view/request commit checks head/task/policy and source/capability/authorization revisions with a bounded retry count, after which the candidate is discarded or the caller receives a typed error. (Verification: cancel, fork, concurrent new-message, and CAS-exhaustion fixtures.)

## Positive Consequences

Compaction becomes verifiable and retrievable: compaction leaves original facts intact under the retention/deletion contract, sibling views are independent, and the model sees a bounded derived summary plus a faithful tail. Splitting can be reasoned about through one closure rule instead of message-role heuristics.

## Negative Consequences & Trade-offs

Source manifests, task revisions, and dependency versions add schema complexity in exchange for verifiable recovery and invalidation semantics. LLM summaries still risk semantic loss, so critical facts must be protected programmatically. Checkpoint construction costs tokens and time; that work is explicitly bounded and cancellable but not free.

## Rejected Alternatives & Negative Knowledge

### Mutating the causal graph to insert summaries

Implementation is locally simpler, but it changes the parent edges of executions that already happened, widening branch-sharing and recovery risk. A summary is a derived representation of history, not an execution that happened; a view manifest expresses that directly.

### Treating a cancellation row or a source ID as sufficient proof

Cancellation closes result acceptance, not necessarily the external action. A source ID proves a reference exists, not that its constraint is visible to the model. Keep unknown outcomes and full effective constraints in deterministic mandatory state, and validate source dependencies at commit.

### Byte-slice fallback for over-budget summaries

Arbitrary byte slicing corrupts UTF-8 and drops mandatory state silently. The fallback is a deterministic structured template that references remaining evidence.

### LLM-only summary validation

A schema check cannot prove semantic correctness, and a model statement of completeness is not evidence. Deterministic logic owns the critical fields.

## Links

- Sibling decisions: [ADR-0275](0275-immutable-execution-facts-and-branch-local-context-views.md), [ADR-0277](0277-unified-context-planner-and-request-compiler.md)
- Related records: [ADR-0254](0254-epistemic-observation-lifecycle-ingestion-folding-and-near-tail-invalidation.md), [ADR-0255](0255-session-ir-native-causal-compaction-and-legacy-dual-track-eradication.md)
- External reference: [Rust `str` slicing](https://doc.rust-lang.org/std/primitive.str.html#impl-Index%3CRange%3Cusize%3E%3E-for-str)
