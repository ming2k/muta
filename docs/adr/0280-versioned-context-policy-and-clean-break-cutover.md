# 0280. Versioned Context Policy and Clean-Break Cutover

- **Status:** Proposed
- **Date:** 2026-09-21
- **Last revised:** 2026-09-22 (proposal contract review)
- **Scope:** configuration, `muta-contracts`/`muta-agent` policy types, offline migration tool, runtime cutover
- **Deciders:** Project maintainers
- **Implementation:** Not implemented by this proposal
- **Evidence baseline:** Working tree at `9cd2b5de`. Source locations are code spans, not links.
- **Builds on:** [ADR-0275](0275-immutable-execution-facts-and-branch-local-context-views.md), [ADR-0276](0276-bounded-raw-artifact-capture-and-durable-publication.md), [ADR-0277](0277-unified-context-planner-and-request-compiler.md), [ADR-0278](0278-compaction-as-a-view-commit.md), [ADR-0279](0279-scoped-inspect-retrieval-retention-and-deletion.md)
- **Amends:** the unversioned `compaction.*` configuration family and the schema/behavior it drives

## Context and Problem Statement

Context behavior is currently configured through an ad-hoc `compaction.*` family whose names are history, not design: `utilization`, `target_utilization`, and `prune_utilization` are ratios of the model **window** `W`, while the target architecture budgets against the **input ceiling** `I` resolved by the model budget contract ([ADR-0277](0277-unified-context-planner-and-request-compiler.md)); a shared window uses `W - O - F`, additionally constrained by any independent input limit. Renaming a key would therefore silently change behavior, and `preserve_rounds` implies a hard protection quantity that the target design explicitly rejects. Several keys are already documented as removed or ignored (`max_active_tokens`, `prompt_reserve_tokens`), and one former key (`compaction_preserve_turns`) has no migration path.

A refactor of this size also needs a defined exit: existing databases and clients must convert once, and the runtime must not carry two execution paths "just in case". This decision fixes the configuration surface, the numeric remapping, the one-time migration, and the mandatory retirement of the legacy context paths.

## Current Configuration Evidence

From `docs/reference/configuration.md` (code spans, not links):

| Legacy key | Current documented value | Semantics |
|---|---|---|
| `compaction.utilization` | `0.85` | Ratio of the model window `W` |
| `compaction.prune_utilization` | `0.65` | Ratio of `W` |
| `compaction.target_utilization` | `0.25` | Ratio of `W` |
| `compaction.fallback_window_tokens` | `32000` | Used when `W` is unknown |
| `compaction.preserve_rounds` | `6` | Rounds kept (read as a hard protection quantity) |
| `compaction.summarize` | `true` | Enable summarization compaction |
| `compaction.prune` | `true` | Enable cheap tool-result pruning |
| `compaction.prune_protect_tokens` | `6000` | Recent tool results protected from pruning |
| `compaction.max_active_tokens`, `compaction.prompt_reserve_tokens` | documented removed/ignored | Legacy residue |
| `compaction_preserve_turns` | former key | No migration path |

## Decision Drivers

1. A configuration name must express its unit and its reference point, so that a rename cannot silently change behavior.
2. One versioned policy must drive one planner ([ADR-0277](0277-unified-context-planner-and-request-compiler.md)); synonym aliases and dual reads are forbidden.
3. Existing data must have a verifiable, offline, one-time migration with an offline rollback and no dual-write runtime.
4. After cutover the runtime graph and every provider call entry point must contain no legacy execution path.

## Considered Options

| Option | Verdict |
|---|---|
| Keep `compaction.*` names and add new keys alongside | Rejected: two vocabularies, silent semantic mismatch, permanent maintenance. |
| Rename keys 1:1 without remapping | Rejected: changes behavior because the reference point changed from `W` to `I`. |
| One versioned `context` policy + explicit offline conversion classification + clean-break retirement | Recommended: preserve semantics where representable and require a reviewed resolution otherwise. |

## Decision Outcome

### 1. One versioned policy

Configuration converges on one versioned `context` policy: request reserve and watermarks, observation preview budget, checkpoint budget, media leases, capture quotas, retention, and inspect page limits. The current `compaction.*` keys are mapped only by the offline migrator; the runtime rejects legacy keys with a conversion hint and keeps no synonym aliases.

### 2. Initial defaults

These are the starting configuration this proposal selects; they are not current defaults and not measured production results. Implementation acceptance may adjust the numbers but must preserve the invariants.

| Setting | Proposed initial value | Boundary semantics |
|---|---|---|
| `soft_watermark` / `hard_watermark` / `target_watermark` | 0.65 / 0.85 / 0.50 of the input ceiling `I` | 0.85 must attempt strong reclamation; a real send always stays within `I`; mandatory blocks may make the target unreachable |
| `output_reserve_tokens` | The total reserve resolved by ADR-0277’s model budget contract, counting reasoning once | Resolve declared limits/defaults and actual wire controls first. A shared-window estimate may use `min(8192, floor(W/8))` only as a provenance-marked total-output policy fallback; separately additive reasoning needs its own bound/estimate. Missing usable accounting returns `BudgetContractUnavailable` |
| `framing_reserve_tokens` | `max(1024, ceil(0.03 * B))`, where `B` is the applicable shared-window or independent-input limit | Apply margins under ADR-0277 without charging the same framing twice; refuse admission when `I <= 0`. An estimated limit remains explicitly estimated |
| `observation_preview_tokens` | `min(2048, floor(0.05 * I))` | A per-item ceiling, not permission to accumulate unlimited items; the planner still allocates the total |
| `checkpoint_output_tokens` | `min(4096, floor(0.08 * I))` | Includes structure and source index; returns an explicit failure when it cannot carry mandatory state |
| `inspect_page_tokens` | 2048 default, 8192 maximum | Must also stay within the retrieval budget available to this request |
| `capture_chunk_bytes` / queue capacity | 64 KiB / 16 chunks | At most about 1 MiB of raw pending writes per active stream pipe, plus a fixed reader buffer |
| `capture_bytes_per_execution` | 256 MiB | Finite local stop/drain at the ceiling, marking Truncated and recording whether producer termination is confirmed; remote outcome may remain unknown |
| `session_artifact_quota_bytes` | 2 GiB | Metered by committed logical bytes plus pending reservations; deduplication does not bypass the session quota. Global accounting includes staging/orphans until reclaimed |
| `attempt_diagnostic_ttl` | 24 hours | Applies only to non-fact diagnostic copies; committed execution results still follow session retention |
| `image_lease` / `rehydrated_page_lease` | The current round | Extended by a task-evidence reference or an explicit pin; constrained by the hard budget |
| `checkpoint_call_timeout` / total build budget | 45 s / 120 s | Interrupt pending calls using monotonic deadlines; every map/reduce/repair call shares the total budget and uses non-recursive admission |
| `checkpoint_max_calls` / `checkpoint_max_reduction_depth` | 16 / 4 | One repair is included in these bounds; every reduction must make progress or fall back/fail |
| `checkpoint_total_input_tokens` / `checkpoint_total_output_tokens` | `8 * I_summary` / `8 * checkpoint_output_tokens` | `I_summary` is resolved for the summary route; count repeated input and all map/reduce/repair work, including failed attempts, with checked arithmetic |
| `preferred_recent_rounds` | 6 | A preference, never a hard protection quantity ([ADR-0277](0277-unified-context-planner-and-request-compiler.md)) |

Every size setting validates its bounds and its relations to the others. Raising the capture quota does not cancel a command's original finite-execution constraints ([ADR-0263](0263-axiom-of-linear-causality-and-pure-execution-primitives.md)). When a raw artifact exceeds budget, error diagnostics still survive in a bounded emergency buffer and the execution state, but are marked incomplete.

### 3. Explicit conversion classes and reference budgets

The offline migrator records a reference budget for every affected model/route/effort profile: the old `W`, the resolved new `I`, capability/policy revisions, provenance, and rounding rules. A global policy shared by several profiles must validate against all of them; silently choosing the currently selected model is forbidden. If profiles require incompatible ratios, conversion requires an explicit choice of supported new policy rather than legacy keys or runtime conversion branches.

For a legacy window ratio, the candidate preserving its absolute token threshold is:

```text
candidate_new_ratio = legacy_ratio * W / I
```

This equation is a candidate, not a clamping rule. The migrator evaluates the resulting integer token threshold and the ordering/bounds of all watermarks together. It classifies every key/profile:

| Classification | Required treatment |
|---|---|
| `Equivalent` | The new representation preserves the old integer token threshold or enablement semantics for the recorded reference budget, with valid bounds and ordering |
| `RequiresDecision` | A threshold is outside the new ceiling, ratios conflict across profiles, rounding changes behavior, a hard guarantee becomes a preference, or a budget is unknown; show the old/new behavior and require explicit resolution before cutover |
| `Removed` | Record an ignored/obsolete key and its disposition; if removal changes effective behavior, require an explicit resolution rather than silently discarding it |

For example, `W = 100000`, `I = 70000`, and an old trigger of `0.85 * W` produce a candidate ratio of approximately `1.2143`. Preserving the 85000-token trigger cannot coexist with the 70000-token input ceiling. Clamping is not equivalent migration. The report must propose a valid new threshold or different model budget and obtain an explicit decision; the new hard admission limit cannot be waived.

| Legacy key | New policy field | Mapping rule |
|---|---|---|
| `compaction.prune_utilization` | `soft_watermark` | Classify candidate absolute-threshold conversion per reference profile |
| `compaction.utilization` | `hard_watermark` | Same classification; the real send ceiling remains `I` |
| `compaction.target_utilization` | `target_watermark` | Same classification; validate watermark ordering as a set |
| `compaction.fallback_window_tokens` | `fallback_window_tokens` | Preserve the configured estimate and mark its provenance; it is not a declared model limit |
| `compaction.preserve_rounds` | `preferred_recent_rounds` | `RequiresDecision`: a preference cannot preserve a hard protection guarantee |
| `compaction.summarize` | checkpoint enablement | Preserve enablement; disabling checkpoints does not waive hard admission |
| `compaction.prune` | lightweight degradation enablement | Preserve enablement; mandatory blocks still cannot be discarded |
| `compaction.prune_protect_tokens` | recent-observation protection budget | Preserve absolute tokens when representable; incompatible guarantees require a decision |
| `compaction.max_active_tokens`, `compaction.prompt_reserve_tokens`, `compaction_preserve_turns` | — | Explicit `Removed` or `RequiresDecision` disposition based on the source version's effective behavior |

The report contains every change, old and proposed token thresholds, scope, reason, and recorded resolution. Unresolved changes block the primary-pointer switch. After conversion the runtime reads only the versioned new policy; the report remains an audit artifact outside its decision path. Equivalence applies only to the recorded reference budgets. Future model/effort/capability changes resolve new budgets under ADR-0277 and do not promise preservation of a historical absolute threshold.

### 4. One-time migration protocol

1. Stop writes from the old version and create a consistent database backup plus artifact inventory; copying a single SQLite file that is still being written does not count as a complete backup.
2. Generate the new database with a standalone offline conversion command. Legacy format parsing never enters the normal daemon startup or request path.
3. For each session, read the legacy transcript, directives, IR, artifacts, and branch material, and build an explicit mapping. When two sources conflict, produce a conflict report and quarantine that session; never silently pick "the newest one".
4. Convert historical calls/results, termination, branches, summaries, tasks/requirements, and artifact references. Where legacy data lacks scope, version, or raw material, write `Unknown` / `Unavailable`; never fabricate execution success, raw text, or historical branches.
5. Compare recoverable fact counts, content hashes, call pairing, branch leaves, summary provenance, and inspect output. A legacy lossy projection migrates only as a known-missing state and is never marked Complete.
6. Verify recovery and request compilation against the new database and emit a machine-readable report; switch the primary database pointer and schema version only after every session has been converted or explicitly handled by the user and every `RequiresDecision` policy item has a recorded resolution. The database, artifact inventory, new configuration, and schema/policy versions form one validated migration generation; publish an atomic generation selector after all are durable so a crash cannot select a mixed set.
7. The new runtime rejects legacy schema and configuration, and the frontend updates per the project's protocol-version rules. Rollback means shutting down the new version and restoring the matching database backup and old binaries; an old program must never open the new database, and facts added after the switch must first be exported or explicitly handled.
8. Legacy backups follow an explicit, bounded retention policy disclosed in deletion reports; expiry permits collection only after the recorded rollback window and any authorized hold have been handled. The conversion tool may remain as an isolated maintenance tool, but is never referenced from the runtime graph, a dual-write path, or a failure fallback.

### 5. Mandatory retirement list

| Current mechanism | Final disposition / replacement |
|---|---|
| `crates/muta-persistence/src/session/ir_bridge.rs` | Deleted from the runtime entirely; format conversion lives only in the offline migration tool |
| `SessionData.transcript` / `Transcript` / `ProjectionDirective` as authority paths | Deleted; historical UI reads facts, and the model compiles from the `ContextView` |
| The translation and rebuild fallback in `commit_context_projection` | Deleted; the atomic validated view/request commit writes no execution facts |
| Legacy `SessionData` synchronization and error-swallowing fallback in `commit_session_ir` | Deleted; one revisioned commit |
| A writable `Vec<Message>` as a second session state | Deleted; only short-lived compiled output and immutable snapshots of committed facts are allowed |
| `MidTurnPruneProjectionGate`, pre-round pruning, and fixed-parameter compiler policy | Merged into the single `ContextPlanner`; the old gate and duplicate threshold entry points are removed |
| Compactor reparenting / the global `compaction_horizon` | Deleted; branch view + checkpoint source manifest ([ADR-0278](0278-compaction-as-a-view-commit.md)) |
| Success-fact derivation from call-intent `FileOperations` | Replaced by confirmed execution effects ([ADR-0275](0275-immutable-execution-facts-and-branch-local-context-views.md) §1) |
| Parallel compaction implementations not wired into the main path, such as `fold_historical_observations` | Deleted, or absorbed for valuable rules and then deleted; no spare strategy remains |
| Post-execute `SpillMiddleware` and internal log-path hints | Deleted; entry-point `ArtifactWriter` + scoped inspect handles ([ADR-0276](0276-bounded-raw-artifact-capture-and-durable-publication.md)) |
| Offstream transcript/IR sequential fallback reads | Deleted; canonical artifact / fact indexes ([ADR-0279](0279-scoped-inspect-retrieval-retention-and-deletion.md)) |
| The mixed-semantics `OffstreamStatus` | Replaced by independent execution/capture/availability/validity fields |
| Legacy `compaction.*` aliases and transitional switches | Deleted after offline conversion; only the new versioned policy remains |

A code-deletion check must not merely grep for symbol strings: historical ADRs, the migration tool, and negative tests may legitimately mention old names. The check must verify that the production dependency graph and every provider call entry point carry no legacy execution path.

### 6. Phased implementation

The phases below are an implementation work breakdown; they authorize neither immediate runtime changes nor deletion of user data on submission of this proposal. Phases may be developed on unreleased branches, but the final release must not carry two online execution paths.

The release requires all six contracts. ADR-0275 is the domain foundation; ADR-0276 supplies durable capture and publication ownership; ADR-0277 owns admission and budget contracts; ADR-0278 supplies checkpoint construction within that admission protocol; ADR-0279 supplies retrieval/revocation/retention; this record owns migration and retirement. The planner and checkpoint builder share ports but have no recursive runtime dependency for summary admission. Policy may disable optional LLM summarization, but cannot disable authority, durability, budget, revocation, or execution-safety invariants. The required inspection panel is a consumer of these contracts, never a separate enforcement path.

Before broad UI expansion, implement one vertical journey across the new path: execute, persist, compact, restart, retrieve the original result, fork, and delete. Use it to expose contract gaps while development remains isolated. Partial scaffolding or unit fixtures alone do not satisfy the release gate.

| Phase | Deliverable | Gate to the next phase |
|---|---|---|
| A. Domain contracts | Typed IDs, facts/effects, the lifecycle axes, branch view, artifact/error protocols | State-transition and source-authority fixtures pass |
| B. Durable substrate | Canonical schema, revisioned writer, complete hydration, chunked capture, publication leases and quota reservations | Crash matrix, publication/GC races, raw retrieval, and scope authorization pass |
| C. Unified projection | Agent planner/compiler, model budget contract, media leases, validated request commit and dispatch authorization | Ordinary/over-long rounds, inclusive/additive budgets, model window shrink, and tool pairing pass |
| D. Checkpoints | History-edge-preserving summaries, source manifests, task constraints, concurrent CAS | Repeated compaction, cancellation, fork/resume, and summary failure pass |
| E. Retrieval and retention | Indexed paging, rehydration leases, dependency invalidation, revocation fences, frontend diagnostics, purge/GC | Large artifacts, concurrent retrieval/deletion/send, revoked late responses, and shared references pass |
| F. Cutover | Offline migration, version switch, legacy code deletion, documentation convergence | The migration report and retirement list above are complete |

### 7. Validation and release gates

This section describes tests and acceptance work that must be implemented; it does not claim they exist or pass today.

| Scenario | Required observation |
|---|---|
| Hundreds of tool interactions in a single round | Execution continues after compaction at safe boundaries; constraints and open work survive; no tool is replayed |
| At least 10 consecutive summarizations, with CJK/emoji/long identifiers | No panic; explicit preservation of required raw text and IDs; the fallback does not grow recursively |
| New and old test scopes differ | They are not marked superseded against each other; an older failure after a change is reported as needing revalidation |
| Refused, failed, and partially applied tools | File inventories record attempted/failed/confirmed effects separately |
| Many concurrent tools plus accompanying images | No projection breaks execution closure or valid wire pairing |
| Dispatched but uncommitted result at interruption | Restart reports `OutcomeUnknown` and does not re-run automatically |
| New message / fork / branch switch during compaction | The old revision's view cannot commit; sibling branch fact hashes are unchanged |
| Crash at every write boundary | Complete recovery of either the old or the new revision; no success state points at missing raw material |
| Over-long single line, ANSI, non-UTF-8, mixed stderr | Memory stays bounded; captured bytes are verifiable; the preview carries a handle and a completeness marker |
| Slow disk, full disk, artifact quota, timeout | Finite termination with an explicit incomplete state; no unbounded queueing and no false claim that raw material exists |
| Cross-round image comparison | The task lease keeps the original images it needs; after the lease ends they can be rehydrated on demand |
| Switching from a large to a small window | The entire input is re-budgeted (including reasoning reserve); an unsatisfiable case produces an explicit admission error |
| Summary LLM timeout/error/over-length/empty response | Bounded fallback or an explicit inability to admit; the previous view remains recoverable |
| Raw material containing a prompt injection | Retrieval/summarization keep tool-source authority and do not generate higher-privilege commands |
| GC concurrent with fork / inspect / append | Artifacts with references or valid read leases are not wrongly deleted |
| Delete a requirement fact under an open task | The task revision closes/cancels; derived summaries, indexes, and request copies are invalidated together |
| Delete source content | Local summaries, indexes, and request copies are invalidated or removed together; shared and backup boundaries are reported accurately |
| Legacy database conflict or missing raw material | The migration report states the blockage or gap explicitly and never fakes recoverability |
| Legacy configuration conversion | Every key/profile is classified with reference budgets; impossible equivalence, rounding changes, and hard-to-preference conversion block cutover until resolved |
| Cancel racing a tool result, then receive a late result | Exactly one ordinary result/closure wins; late evidence appends once, does not advance the new cursor, and unknown effects are never replayed |
| Closed prefix followed by a wholly preserved open execution group | Earlier groups may compact; the open group stays intact and unknown outcomes remain mandatory |
| Inclusive output cap versus additive reasoning, independent input limit, unknown effort mapping | Reasoning is charged once, controls match reserves, and unknown accounting is explicit or refused |
| Summary inputs overflow through map/reduce/repair | No recursive checkpoint admission; all work shares call/token/depth/deadline bounds and no-progress detection |
| Mandatory constraint appears only as an ID in generated narrative | Validation cannot mistake ID coverage for visible constraint preservation; deterministic mandatory content remains present or admission fails |
| Publish a blob while GC runs before reference commit | A valid publication lease protects it; fenced/expired publishers cannot attach references to reclaimed generations |
| Concurrent captures reach session/global quota | Reservations prevent oversubscription; failures release capacity and preserve honest incomplete state |
| Delete after compile but before send, page delivery, or retry | Revocation wins the relevant fence and prevents delivery/handoff; storage leases do not grant stale access |
| Delete while a provider request or checkpoint build is in flight | Dependent responses cannot commit content or execute tools; already handed-off requests are reported as potentially exposed |
| Late evidence or cached import arrives after source deletion | Revoked payload cannot resurrect; only authorized content-free recovery metadata may remain |
| Crash during migration-generation publication | Startup selects the complete old or new database/artifact/configuration generation, never a mixed set |

Verification order follows the project rules: run `cargo check -p <package>` for the changed crate first, and always run unit/integration tests with `cargo nextest run -p <package> -E 'test(<specific_filter>)'`. Filters must reference real test names at implementation time; this proposal does not provide plausible-looking commands for tests that do not exist. This documentation-only change requires no Rust compilation.

Each invariant requires an evidence entry naming its production entry point, real targeted test or acceptance journey, verified revision, and result. Type definitions, mock-only scaffolding, prose assertions, and a passing symbol grep cannot substitute for a verified production path. Record peak capture/retrieval memory, writer transaction latency, and p50/p95 planning latency under declared workloads; regressions must be resolved or explicitly justified without weakening durability or authorization.

Final user acceptance starts cold from a real terminal/web/public API and walks the full journey: read, modify, verify a failure, modify again, verify success, compact, exit, resume, retrieve the original failure, fork, and delete a specified artifact. Fault injection belongs to separate automated tests and is not passed off as user acceptance through a private entry point.

The release gate requires verification evidence for every invariant in [ADR-0275](0275-immutable-execution-facts-and-branch-local-context-views.md) through this record, the key journeys above passing, an auditable migration report, a completed runtime retirement list, and complete documentation and protocol updates. "Delete the bridge later" or "add raw retrieval later" cannot remain as post-release follow-up work.

### 8. Documentation convergence

On acceptance, implementation updates `docs/architecture/session-ir.md`, the context/pruning/compaction/persistence explanation pages, the configuration/tool/protocol references, and the development-test and acceptance documentation. Historical records are marked and archived under the governance rules rather than having their accepted decision outcomes rewritten. The final blueprint must describe the implemented call chain.

## Invariants & Behavioral Boundaries

- **`[INV-POLICY-01]` Single Versioned Policy**: One versioned `context` policy drives admission; the runtime rejects legacy `compaction.*` keys with a conversion hint and keeps no synonyms or dual reads. (Verification: startup/config fixtures; legacy keys are refused, not silently accepted.)
- **`[INV-POLICY-02]` Explicit Conversion Classification**: Offline migration classifies every key/profile as `Equivalent`, `RequiresDecision`, or `Removed` against recorded reference budgets. Clamping, unknown budgets, rounding differences, and hard-to-preference changes cannot claim equivalence; unresolved behavior changes block cutover. The runtime retains no legacy conversion path. (Verification: every legacy key, incompatible thresholds, multi-model profiles, unknown budgets, and decision-report fixtures.)
- **`[INV-POLICY-03]` Auditable One-Time Migration**: Migration uses a consistent backup plus an offline converter outside the runtime path; conflicts are quarantined, missing material becomes `Unknown`/`Unavailable` and is never fabricated, and an atomic generation selector prevents mixed database/artifact/configuration activation; the old runtime never opens the new database. (Verification: conflict, missing-material, generation-switch crash, and rollback fixtures with a machine-readable report.)
- **`[INV-POLICY-04]` No Residual Dual Path**: After cutover the runtime dependency graph and every provider call entry point contain no legacy execution path, dual write, or compatibility fallback; retirement is verified by the dependency graph, not by symbol grep. (Verification: dependency-graph assertion and provider-entry audit.)

## Positive Consequences

Configuration becomes self-describing and safe to change, migration is verifiable and reversible, and the codebase loses the compatibility burden that would otherwise tax every future change to read, write, recovery, deletion, and diagnosis.

## Negative Consequences & Trade-offs

The one-time cutover breaks compatibility with older clients and databases; there is no twin-run period. The remap changes some numbers, and users who pinned `preserve_rounds` lose a hard guarantee in favor of a preference. Both costs are deliberate and bounded, replacing unbounded long-term dual-path maintenance.

## Rejected Alternatives & Negative Knowledge

### Permanent dual-track compatibility

Dual tracks multiply the cost of every capability across read, write, recovery, deletion, and troubleshooting. Use offline conversion and explicit version rejection, and keep only the new model in the final runtime.

### In-place, lazy schema upgrade

Lazy upgrades leave two representations alive indefinitely and make "which code path ran" unknowable. A single offline conversion with a hard version gate is auditable.

### Clamping an impossible conversion and calling it equivalent

An old threshold can lie above the new input ceiling; a single global ratio can also fail to preserve thresholds across different models. Clamping hides a behavior change. Record explicit conversion classes and require a decision while preserving the new hard bounds.

### Shipping partial contracts behind permanent fallback paths

A new planner with legacy persistence, or durable capture without safe retrieval/revocation, does not satisfy the lifecycle. Develop in bounded phases, then release the verified vertical path and remove the old runtime graph in the same cutover.

### Silent key renaming

Renaming `utilization` to a new key without remapping changes behavior because the reference point changed. The migrator must remap by semantics and report every change.

## Links

- Sibling decisions: [ADR-0275](0275-immutable-execution-facts-and-branch-local-context-views.md), [ADR-0276](0276-bounded-raw-artifact-capture-and-durable-publication.md), [ADR-0277](0277-unified-context-planner-and-request-compiler.md), [ADR-0278](0278-compaction-as-a-view-commit.md), [ADR-0279](0279-scoped-inspect-retrieval-retention-and-deletion.md)
- Related records: [ADR-0187](0187-persistence-v2-incremental-append-and-blob-reference-ledger.md), [ADR-0196](0196-supervised-persistence-writer-and-typed-persistence-errors.md), [ADR-0231](0231-one-door-to-the-database.md)
- Configuration reference to update on implementation: [`docs/reference/configuration.md`](../reference/configuration.md)
