# 0277. Unified Context Planner and Request Compiler

- **Status:** Proposed
- **Date:** 2026-09-21
- **Last revised:** 2026-09-22 (proposal contract review)
- **Scope:** `muta-agent` (planner), `muta-contracts` (ports, token accounting), provider lowering
- **Deciders:** Project maintainers
- **Implementation:** Not implemented by this proposal
- **Evidence baseline:** Working tree at `9cd2b5de`. Source locations are code spans, not links.
- **Supersedes / Amends:** pre-round pruning, the `MidTurnPruneProjectionGate`, and the fixed-parameter budgeting pass in the contracts compiler
- **Sibling decisions:** [ADR-0275](0275-immutable-execution-facts-and-branch-local-context-views.md), [ADR-0276](0276-bounded-raw-artifact-capture-and-durable-publication.md), [ADR-0278](0278-compaction-as-a-view-commit.md), [ADR-0279](0279-scoped-inspect-retrieval-retention-and-deletion.md), [ADR-0280](0280-versioned-context-policy-and-clean-break-cutover.md)

## Context and Problem Statement

The same provider request is currently shaped in several places: a pre-round pruning step, a mid-turn projection gate, and a fixed-parameter budgeting pass inside the contracts compiler. These paths disagree on semantics, use hard-coded thresholds, and each can trim history, so no single component can prove what the model received or that a budget was honored. Staleness detection is entangled with budget degradation: `has_stale_tool_results` is literally "is the degradation plan non-empty", so unrelated edits or build/test commands can trigger command staleness.

This decision collapses every provider request to **one admission path**: a typed planner produces an operation plan, a pure compiler lowers it to an immutable request, and no component downstream may trim history again.

## Current Implementation Evidence

| Location / symbol (code span) | Current observation | Required change |
|---|---|---|
| `crates/muta-agent/src/orchestration.rs`, `MidTurnPruneProjectionGate::project_context`, `execute_round` pre-round pruning | Isolated pruning/compaction entry points run before the round and mid-turn; the gate mainly prunes and commits a projection. | One planner; in-round compaction only at a safe commit boundary ([ADR-0278](0278-compaction-as-a-view-commit.md)). |
| `crates/muta-contracts/src/pressure.rs`, `has_stale_tool_results`, `plan_prune`, `prune_tool_results` | `has_stale_tool_results` == `!plan_prune(...).is_empty()`; staleness and degradation share one plan. | Separate validity classification from budget degradation; key on command scope and resource version. |
| `crates/muta-contracts/src/session_ir/compiler.rs`, `pass2_context_budgeting` | Applies `prune_tool_results(&mut messages, 4_000, 100)` with literal thresholds and slices body text at a byte ceiling (`&msg.content[..max_tool_chars]`). | Delete the separate policy; execute only the unified plan and cut at safe text boundaries. |

## Decision Drivers

1. One component must be able to prove the exact bytes the model received and that every budget was honored.
2. Freshness must be evaluated independently of budget; the two must never be inferred from each other.
3. Determinism and cache stability are optimization and audit properties, not correctness substitutes for validity or authority.
4. A no-progress planning loop must terminate with a typed error, not silently delete constraints.

## Considered Options

| Option | Verdict |
|---|---|
| Keep adding separate rules to pre-round pruning, the mid-turn gate, and the compiler | Rejected: entry points stay fragmented; persistence and budget consistency cannot be proven. |
| Delegate trimming to the provider adapter | Rejected: hides history mutation behind an I/O layer and defeats request-snapshot auditability. |
| Let the model decide what to forget | Rejected: correctness must not depend on model economization. |
| One planner + one pure compiler at a single admission path | Recommended. |

## Decision Outcome

A final provider call accepts only a compiled request snapshot. The planner produces a typed operation plan; the compiler is a deterministic function with no I/O, no history mutation, and no second trimming pass.

```text
snapshot(branch/task/policy/capability revisions, resource and authorization versions)
  -> validate authority and execution closure
  -> evaluate freshness independently of budget
  -> reserve mandatory blocks and output capacity
  -> select representations within component budgets
  -> build checkpoint if necessary and permitted  (ADR-0278)
  -> validate source coverage / references / wire groups
  -> compile candidate request and manifest
  -> provider-specific lowering and final budget validation
  -> atomically commit view + manifest with dependency preconditions
  -> authorize dispatch against current revocation state (ADR-0279)
  -> send the validated snapshot
```

Failed compilation or lowering leaves the active view unchanged. Candidate construction and lowering run outside the writer transaction. The commit verifies the pinned branch/task/policy/capability and authorization revisions and atomically installs the view, checkpoint, request manifest, and artifact references. A conflict discards or replans the candidate within the finite bound. Ephemeral calls use a scoped request commit without mutating conversational facts or the branch view. Downstream adapters can reject an invalid request but cannot trim, reorder semantically significant blocks, or substitute sources to make it fit. Any such change requires a new plan and request revision.

### 1. Model budget contract

The selected model/route supplies a versioned budget contract rather than a universal interpretation of vendor parameter names. Each quantity records whether it is declared, locally configured, or estimated, along with its source and calibration version. The contract specifies:

- a shared input/output window, an independent input limit, or both;
- the effective output cap and whether it includes reasoning;
- any separately bounded visible-output and reasoning components, and how they consume the window;
- the actual generation controls sent on the wire and their relation to the reserves;
- tokenization/media estimation rules and uncertainty margins.

An effort ladder ([ADR-0270](0270-decouple-reasoning-effort-ladders-from-core-contracts.md)) declares supported effort levels; it is not itself a numeric reasoning budget. A numeric mapping needs separate capability metadata and provenance. Qualitative effort must not be presented as a guaranteed token upper bound.

For a shared window `W`, let `O` be the resolved total output reserve that consumes that window, counting reasoning exactly once. `F` covers framing not already included in measured blocks and estimation uncertainty; each framing component is charged once:

```text
shared_input_ceiling = W - O - F
I = min(shared_input_ceiling, declared_input_limit - input_margin)
    (apply only limits present in the budget contract)
```

If only an independent input limit is declared, reserve its input margin without inventing a shared window or subtracting an unrelated output cap. When the effective generation cap already includes reasoning, `O` is that cap, not that cap plus reasoning again. Only a contract with separate additive components uses `O = visible_output_reserve + reasoning_reserve`. Validate independent output limits and the consistency of wire controls as well as the input ceiling. Non-positive ceilings and contradictory contracts return typed admission errors.

Unknown accounting uses an explicitly configured conservative estimate with provenance and margin; if none is available, return `BudgetContractUnavailable`. Estimation is not proof of the provider's hidden accounting. The local guarantee is that every block fits the resolved contract; any provider size rejection is recorded, updates calibration, and may trigger bounded re-planning as a new revision, never silent trimming or side-effect replay.

The input budget covers system/project instructions, tool schemas, the current user input, task state, summaries, the conversation tail, tool observations, images, and temporary injections. All use the same accounting interface. The final lowered content is checked with the same contract, including added protocol framing and its reserved coverage; validation may refuse but cannot mutate the plan. Reported usage calibrates future estimates and never substitutes for measuring the next request. Budget resolution and validation run for every call after model, effort, or capability changes, not only at configuration load.

### 2. Protection order

1. Authority- and protocol-mandatory blocks, the current user request, and the protocol structure of open tool calls.
2. Explicit constraints of active tasks, unresolved items, and the evidence the next action needs.
3. The most recent complete execution groups and the observations they reference.
4. Summaries of completed phases and historical indexes.
5. Older detail that can be re-queried.

A pin expresses high retention priority, not an exemption from the hard budget. If the mandatory blocks alone exceed `I`, the system returns `ContextAdmissionExceeded` with per-component metering; it neither silently deletes constraints nor compacts in an unbounded loop. The caller may choose a larger window or narrow the input.

### 3. Degradation order and hysteresis

The lightweight degradation order is deduplication, deterministic folding, excerpting, reference-only, and phase summary. The policy weighs expected reclamation, what the next step needs, and cache-rewrite cost, but there is no special case that keeps invalidated content forever for the sake of the cache.

The budget policy uses a soft watermark, hard admission, and a target watermark to form hysteresis, and the target includes the checkpoint itself plus fixed overhead. How many rounds to retain is only a preference and never replaces the token or execution-closure constraints. A single planning pass has count and time limits; when it makes no progress it returns an explicit error immediately.

### 4. Provider caching is a consequence, not a contract

Provider caching keys on the actually serialized content. Sort tools stably, keep instruction order fixed, and avoid placing timestamps in the prefix; record local block fingerprints to locate changes, without promising a cache hit across models or providers ([ADR-0217](0217-request-components-and-derived-cache-plan.md)). Anthropic's documentation is explicit that a prefix byte change can invalidate the cache; see [Cache diagnostics](https://platform.claude.com/docs/en/build-with-claude/cache-diagnostics). When a checkpoint is regenerated from an LLM summary, the resulting prefix change is recorded rather than promised stable ([ADR-0278](0278-compaction-as-a-view-commit.md)).

### 5. Core ports

The recommended shape of the admission-side ports follows; these are unimplemented type contracts, omitting Rust generics and async details:

```text
ContextPlanner.plan(snapshot, request_intent, model_budget) -> ContextPlan
CheckpointBuilder.build(plan, source_snapshot) -> ValidatedCheckpoint   (ADR-0278)
RequestCompiler.compile(snapshot, candidate_view, capabilities) -> RequestCandidate
ProviderLowering.validate(candidate, budget_contract) -> ValidatedRequestCandidate
SessionWriter.commit_request(expected_revisions, operation_id, candidate)
    -> CommittedRequestSnapshot | RevisionConflict | PersistenceFailure
DispatchAuthority.begin(snapshot, current_authorization) -> DispatchPermit | Revoked
```

The provider port accepts a committed `RequestSnapshot` and a dispatch permit, never a caller-constructed history array. A permit is consumed at the transport handoff under ADR-0279; it is not reusable authority for a retry. Test and ephemeral summary requests also pass through the same compilation admission entry point, expressed as an explicit ephemeral scope, and cannot bypass budgets or authorization by claiming to be an "internal call". Usage reported by such ephemeral calls is recorded separately from conversational accounting and never pollutes task or session statistics ([ADR-0275](0275-immutable-execution-facts-and-branch-local-context-views.md) §7).

### 6. Non-recursive internal requests

Request intent is typed. Conversational admission may request a checkpoint; checkpoint map/reduce/repair calls use `CheckpointSummary` intent, which cannot invoke `CheckpointBuilder`. Title and other bounded auxiliary intents likewise cannot mutate conversational views or trigger compaction. All intents still use the same authorization, accounting, compiler, and final validation path.

The checkpoint builder partitions sources deterministically within input budgets. It has total input/output, call-count, reduction-depth, and monotonic elapsed-time bounds shared across map, reduce, and repair calls. Each reduction must measurably reduce the remaining material; exhaustion or no progress chooses the bounded deterministic fallback or a typed failure. Nested calls cannot reset the parent's budgets. Reserve each call's input and maximum output cost before dispatch; unknown usage after a timeout consumes the reservation rather than restoring a potentially spent budget. An oversized summary request is split within those limits or refused, never sent back into recursive checkpoint planning.

### 7. Snapshot identity and downstream invalidation

Determinism is defined over all content-affecting inputs: committed source and view versions, task/policy/capability versions, instructions, tool schemas, temporary blocks, media references, and compiler/lowering versions. Record them in the manifest. Credentials, signatures, transport timestamps, and other attempt-local envelope fields are excluded from the deterministic context-block identity and are never persisted as secrets in the manifest.

A committed snapshot is immutable but not an irrevocable permission to send. Every initial dispatch and retry follows [ADR-0279](0279-scoped-inspect-retrieval-retention-and-deletion.md#7-revocation-and-in-flight-work). Invalidated source dependencies require re-planning; revoked material cannot be sent merely because compilation happened before deletion.

## Invariants & Behavioral Boundaries

- **`[INV-ADMIT-01]` One Admission Path**: Every provider request — including title/summary/test/ephemeral calls — compiles through the same planner and compiler and passes authority and budget admission; "internal call" is not a bypass. (Verification: provider port contract and wire fixtures; an attempted bypass fails.)
- **`[INV-ADMIT-02]` Complete Budget Accounting**: A versioned model budget contract counts every input block and each output/reasoning component exactly once, validates the final lowering and wire generation controls, and distinguishes declared bounds from estimates. Effort labels alone supply no numeric guarantee; unsupported accounting fails explicitly. (Verification: inclusive versus additive output caps, independent versus shared windows, unknown effort budgets, media uncertainty, and provider size rejection.)
- **`[INV-ADMIT-03]` Determinism Given a Committed View**: Identical source/view revisions and every content-affecting input/version pinned under §7 produce byte-identical context blocks; where an LLM-derived checkpoint is regenerated, the prefix change is recorded, not promised stable. (Verification: determinism comparison and manifest comparison across a restart.)
- **`[INV-ADMIT-04]` Finite Planning**: A planning pass has bounded iterations and time; no progress returns an explicit typed error, and mandatory blocks exceeding `I` return `ContextAdmissionExceeded` with per-component metering rather than silent deletion. (Verification: over-long round, model window shrink, and pathological-input fixtures.)
- **`[INV-ADMIT-05]` No Recursive Checkpoint Admission**: Summary/auxiliary intents cannot initiate checkpoints; all nested summary work shares finite call/token/depth/time bounds and fails or falls back without resetting them. (Verification: over-budget map and reduce requests, no-progress reduction, and repeated repair failures.)
- **`[INV-ADMIT-06]` Validated Request Commit**: Compilation and final lowering validation precede the atomic view/request commit; dependency conflicts, failed compilation, and failed lowering leave the prior view active. Dispatch and retries require current authorization. (Verification: failure injection at each preparation stage and deletion between commit and dispatch.)

## Positive Consequences

There is exactly one place to reason about, test, and instrument request size and ordering. Staleness and budget become independently testable, and the request manifest makes every request auditable after the fact.

## Negative Consequences & Trade-offs

Consolidating on one planner requires migrating three existing entry points in one cutover and deleting their thresholds ([ADR-0280](0280-versioned-context-policy-and-clean-break-cutover.md)); during development the old and new paths must not both be online. A single planner is a hot path, so its performance must be measured (p50/p95 planning latency) rather than assumed.

## Rejected Alternatives & Negative Knowledge

### Freshness inferred from the degradation plan

`has_stale_tool_results == !plan_prune(...).is_empty()` couples two unrelated concerns and spuriously invalidates unrelated evidence. Freshness is a typed classification over scope and resource version ([ADR-0275](0275-immutable-execution-facts-and-branch-local-context-views.md) §6).

### Fixed-parameter pruning inside the compiler

Literal thresholds in the compiler cannot honor a model-relative budget and silently slice text at byte ceilings. Policy lives in the planner; the compiler is pure.

### Universal output-plus-reasoning arithmetic

A field called "max output" does not define whether reasoning is already included or how it shares the input window. Adding reasoning unconditionally can reserve it twice; treating an effort label as a token bound can under-reserve it. Resolve a typed model contract before accounting.

### Recursive summary admission bounded only by timeout

A timeout limits duration but leaves the cyclic dependency intact and can consume calls without progress. A summary intent structurally forbids checkpoint recursion and inherits the parent's finite work budget.

### Deep-history freeze as a correctness rule

A stable prefix has economic value, but providers differ in behavior and cost, and stale evidence can sit deep in the history. A cache policy must not block invalidation propagation; use tail invalidation records and real measurements to decide whether to rewrite the projection.

## Links

- Sibling decisions: [ADR-0275](0275-immutable-execution-facts-and-branch-local-context-views.md), [ADR-0276](0276-bounded-raw-artifact-capture-and-durable-publication.md), [ADR-0278](0278-compaction-as-a-view-commit.md), [ADR-0280](0280-versioned-context-policy-and-clean-break-cutover.md)
- Related records: [ADR-0056](0056-model-context-assembly-boundary.md), [ADR-0061](0061-atomic-model-request-boundary.md), [ADR-0213](0213-model-request-composition-and-context-lifecycle.md), [ADR-0217](0217-request-components-and-derived-cache-plan.md), [ADR-0270](0270-decouple-reasoning-effort-ladders-from-core-contracts.md)
- External reference: [Cache diagnostics](https://platform.claude.com/docs/en/build-with-claude/cache-diagnostics)
