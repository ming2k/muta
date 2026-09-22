# 0279. Scoped Inspect Retrieval, Retention, and Deletion

- **Status:** Proposed
- **Date:** 2026-09-21
- **Last revised:** 2026-09-22 (proposal contract review)
- **Scope:** `muta-runtime` (inspect service, GC), `muta-persistence` (indexes, artifact store), terminal/web context panel
- **Deciders:** Project maintainers
- **Implementation:** Not implemented by this proposal
- **Evidence baseline:** Working tree at `9cd2b5de`. Source locations are code spans, not links.
- **Builds on:** [ADR-0275](0275-immutable-execution-facts-and-branch-local-context-views.md) (facts/views), [ADR-0276](0276-bounded-raw-artifact-capture-and-durable-publication.md) (artifacts)
- **Amends:** the `OffstreamStatus` mixed state, the offstream fallback read paths, and the "permanently lossless" retention claim
- **Sibling decisions:** [ADR-0277](0277-unified-context-planner-and-request-compiler.md), [ADR-0278](0278-compaction-as-a-view-commit.md), [ADR-0280](0280-versioned-context-policy-and-clean-break-cutover.md)

## Context and Problem Statement

Retrieval and deletion share an artifact store but not a contract. The current off-stream retrieval locates IR results by iterating the whole graph node map (unordered) rather than by a stable index or lineage, and folded retrieval walks a mutated `parent_id` chain. A single `OffstreamStatus` conflates execution state, capture completeness, and availability. Retention is asserted as "permanently lossless" with no boundary, and deletion has no defined cascade to derived content or to task state.

This decision makes retrieval a **scoped, paginated, typed** service over a stable index, and makes retention/deletion an **explicit, observable data-lifecycle** operation with defined roots, batches, cascades, and report boundaries.

## Current Implementation Evidence

| Location / symbol (code span) | Current observation | Required change |
|---|---|---|
| `crates/muta-runtime/src/offstream.rs`, `PrunedToolSource` | `enumerate`/`read` scan transcript entries (ordered) and then iterate `ir.history.nodes.values()` — a `HashMap` over all nodes, unordered and not restricted to the active lineage. | Read by a stable, scoped index and source manifest; never iterate the whole node map. |
| `crates/muta-runtime/src/offstream.rs`, `FoldedCausalSource::read` | Walks `parent_id` upward until a previous `Compaction` node to guess coverage. | Read coverage from the checkpoint's source manifest, not a mutated parent chain. |
| `crates/muta-runtime/src/offstream.rs`, `OffstreamStatus` | One enum mixes execution, capture, and availability semantics. | Split into independent `execution_status`, `capture_status`, `availability`, `validity`, `representation` fields. |

## Decision Drivers

1. Retrieval must be authorized by the session/branch that owns the artifact; a handle is an address, not a credential.
2. A read must be bounded in tokens, bytes, and compute, and must never load a whole log or subagent transcript into memory.
3. Deletion must be recoverable, observable, and honest about its boundaries (local vs shared vs exported vs provider-received).
4. GC must never race a live reference or a read lease, and must never silently evict evidence that was promised recoverable.

## Considered Options

| Option | Verdict |
|---|---|
| Keep transcript/IR-order scanning behind an opaque handle | Rejected: unordered, unscoped, and breaks under compaction. |
| A single `OffstreamStatus` for all retrieval state | Rejected: execution, capture, and availability are independent. |
| TTL as the universal expiry and deletion rule | Rejected: TTL cannot express versions, constraints, or unfinished tasks ([ADR-0275](0275-immutable-execution-facts-and-branch-local-context-views.md) §6). |
| Scoped, indexed, paginated retrieval + explicit retention/deletion | Recommended. |

## Decision Outcome

### 1. Handles and schemes

`inspect` remains the unified history-retrieval tool, supporting metadata, paged reads, filtered search, and image rehydration. `call:`, `fold:`, and `sub:` are resource categories, not authorization credentials. A handle contains the session scope and internal execution/checkpoint IDs and must not rely on the global uniqueness of a provider call ID; `artifact:` is added for standalone media/file snapshots.

A pagination cursor binds the content hash, the query, and the revision; changing the query must not reuse an old cursor. Every read has token, byte, and compute-time ceilings, and indexed retrieval must not first load all logs or a subagent transcript into memory. `fold:` reads through the source manifest instead of guessing coverage by walking a mutated parent chain.

Return values carry `execution_status`, `capture_status`, `availability`, `validity`, and `representation` separately. Errors distinguish at least `NotFound`, `NotAuthorized`, `Purged`, `Expired`, `Corrupt`, `Incomplete`, and `CursorMismatch`; they must not all collapse into an empty result.

### 2. Subagent retrieval boundary

The subagent boundary of [ADR-0263](0263-axiom-of-linear-causality-and-pure-execution-primitives.md) and the off-stream boundary rules (`INV-OFFSTREAM-*`) are retained: a subagent returns a synthesized summary and a handle and does not itself receive cross-scope retrieval authority. Cross-session memory enters only through explicitly authorized retrieval import; existing role memory is one retrieval source, not a second active context fact source, and an import record keeps its source, time, and revocable relation.

### 3. Frontend context panel

The frontend offers a unified context inspection panel: per-request component tokens, the current checkpoint, degraded content with reasons, raw-material availability, pin/unpin, retrieval, and storage usage. Ordinary lightweight pruning stays silent; summarization, budget shortfall, persistence failure, and permanent deletion emit explicit events. Expanding or collapsing the UI changes display only and never the model context automatically.

### 4. Retention

Raw observations are `SessionBound` by default and live with the session; disk originals are not deleted automatically to save tokens. Short-lived attempt diagnostics have an explicit deadline. Closing a session is not deleting a session; a deployment may configure an archival deadline and hard quotas, but reaching a quota must **stop new capture with a stated reason and a user-visible event**, and must not silently evict evidence that was still promised recoverable, nor stall the single writer.

### 5. Garbage collection

GC roots include retained sessions/branches, task evidence, checkpoint source manifests, valid request snapshots, user pins, migration backups, in-flight read leases, and publication leases defined by [ADR-0276 §4](0276-bounded-raw-artifact-capture-and-durable-publication.md#4-publication-ownership-and-garbage-collection). A reference does not mean a permanent pin: once a root's retention policy expires, that group of references can be released as a whole.

GC uses bounded mark/sweep batches, generation watermarks, and fenced read/publication leases so it cannot wrongly delete data that races with a new commit, a pending publication, or an `inspect`. Marking produces candidates, not permission to unlink: sweep acquires reclaim ownership and rechecks references and leases against the current generation. Reference/lease acquisition and reclaim ownership serialize through the writer; a publisher or reader cannot attach to a generation already being reclaimed. Deletion state is committed first, then unreferenced chunks are reclaimed; after a crash the same deletion job resumes. A shared blob is physically reclaimed only when every authorized reference is gone.

GC runs both at session close/retention tick and, for long-lived sessions, on a periodic tick; the default batch is at most 1000 objects or 100 ms, saving a continuation and returning as soon as either limit is reached.

### 6. Deletion

When a user deletes specific content, the cleanup scope must cover summaries, full-text indexes, caches, and request copies; derived records may be invalidated and rebuilt wholesale, but deleting only the source blob while the same secret text survives elsewhere is not acceptable. A legitimate reference held independently by another session is handled according to the deletion scope the user chose, and the UI must not present "removed from this session" as "every copy physically erased".

Deleting a user requirement fact cascades to the task state: an open `TaskState` that depends on it is closed or cancelled and its revision is recorded ([ADR-0275](0275-immutable-execution-facts-and-branch-local-context-views.md) §1). A requirement fact is a GC root while its task is open, so it is not collected silently underneath a live task.

Immutable facts do not prevent authorized deletion: keep a minimal tombstone with the structural relations but without the original content, and delete the payload and derived content. Backups, already-exported copies, and requests already received by a provider are outside what local GC can erase immediately; the deletion result must report each boundary separately. An application-level purge does not promise forensic-grade erasure of the storage medium.

### 7. Revocation and in-flight work

The deletion transaction is the access-revocation point. It increments the relevant authorization/dependency revision, installs tombstones, invalidates derived dependents, and enqueues resumable physical cleanup. After this point no new read-page delivery, derived commit, or transport handoff may be authorized against the revoked content. Logical denial is immediate even if physical cleanup takes several batches. The dependency index covers facts, task state, checkpoints, summaries, search indexes, request copies, and authorized cross-scope imports; conservative whole-record invalidation is allowed when finer provenance is unavailable. Revocation uses a bounded root/scope revision update; dependent enumeration and physical cleanup proceed in resumable batches. Readers, dispatchers, and committers must reject stale authorization generations until dependencies are revalidated, so a large cascade cannot require an unbounded writer transaction or leave an access window while cleanup catches up.

Authority checks and use must be ordered, not separated by an unprotected check/use interval:

| Operation | Required ordering and revocation behavior |
|---|---|
| Inspect / image rehydration | Check current authorization when delivering each bounded page/media result. A read lease protects bytes from reclamation but grants no authority after revocation. Already delivered content is reported as outside immediate recall. |
| Checkpoint or request construction | Pin source dependencies; a revoked dependency rejects the final commit. Cancel outstanding builds where possible and remove derived buffers/copies through cleanup. An old valid signature/hash is not fresh authority. |
| Queued request or retry | Recheck at the transport handoff. A permit is scoped to one attempt and consumed once; deleting a dependency before handoff invalidates it. Recompile only from authorized material. |
| In-flight provider request | Fence further sends/retries, request cancellation, and mark the dependent attempt invalidated. A response based on revoked sources cannot commit as a new summary or execute carried tool calls. Discard its content or route minimal content-free outcome metadata to recovery. |
| Late execution evidence / import | Verify the original scope's current authority before accepting payloads. Revocation forbids resurrecting deleted content through reconciliation, deduplication, cached imports, or delayed callbacks. Preserve only authorized, content-free execution/unknown-outcome metadata when needed. |

The dispatch coordinator orders transport handoff with revocation under a short scope fence. A handoff transfers the request to the transport that can emit bytes; it is the externally exposed boundary, even if delivery is not yet confirmed. Revocation either wins first and prevents the handoff, or sees an already handed-off attempt and records it as potentially received externally. There is no claim of atomicity with a remote server. Buffered local sends are cancelled where possible; a crash after handoff is reported as potentially exposed, never definitely unsent. Apply the same ordering to result-page delivery. Do not hold a database writer transaction or the short fence across network waits or unbounded streaming.

Derived records retain source dependencies even if raw text was summarized. A response cannot launder revoked text back into storage by assigning itself a new ID. Access denial takes precedence over cached snapshots and retention pins. Physical reclamation still honors other authorized references and bounded outstanding storage leases; these affect when bytes can be freed, not whether revoked content may be delivered.

Deletion results distinguish access revocation, local cleanup pending/completed, independently retained shared copies, backups/exports, and potentially provider-received requests. Local completion requires clearing owned caches and derived copies; it does not promise erasure of already delivered data or forensic erasure of storage media.

## Invariants & Behavioral Boundaries

- **`[INV-RET-01]` Scoped Authorization**: Retrieval is authorized by session/branch scope; handles (`call:`, `fold:`, `sub:`, `artifact:`) confer no authority and carry internal IDs, not provider call IDs; cross-scope and duplicate-ID reads are denied. (Verification: cross-scope and duplicate call-ID tests.)
- **`[INV-RET-02]` Paged Bounded Reads**: Every read has token, byte, and compute ceilings; a cursor binds content hash, query, and revision; indexed retrieval never loads a whole log or subagent transcript into memory. (Verification: large-artifact paging and cursor-mismatch tests.)
- **`[INV-RET-03]` Observable, Recoverable Deletion**: Deletion commits state first, then reclaims unreferenced chunks; a shared blob is reclaimed only when all authorized references are gone; results are reportable and resumable across a crash; "removed from this session" is never presented as "all copies erased". (Verification: shared-blob, index, snapshot, and crash-resume deletion tests.)
- **`[INV-RET-04]` Non-Interfering GC**: GC uses bounded mark/sweep batches, generation watermarks, and fenced read/publication leases with a final reclaim-ownership check; artifacts with a live reference or valid lease are never concurrently reclaimed; roots are never silently evicted while still promised recoverable; quota exhaustion stops new capture with a user-visible event and does not stall the writer. (Verification: GC racing fork/inspect/append, and quota-exhaustion tests.)
- **`[INV-RET-05]` Deletion Cascade**: Deleting a user requirement fact cascades to open `TaskState` revisions (close/cancel) and to summaries, indexes, caches, and request copies; a requirement fact is a GC root while its task is open. (Verification: delete-requirement-under-open-task fixtures.)
- **`[INV-RET-06]` Revocation Before Reuse**: Deletion serializes with page delivery, derived commits, and each transport handoff; revoked content cannot be newly delivered, sent, retried, or reintroduced through late responses/imports. A lease protects storage, not access authority. Already handed-off requests are reported as potentially externally received. (Verification: deletion racing inspect delivery, queued dispatch/retry, summary commit, late execution evidence, and in-flight provider response.)

## Positive Consequences

Retrieval becomes provable (stable index, scoped authorization, bounded pages) and independent of graph topology. Retention and deletion gain explicit, testable boundaries, and the UI can report exactly what was removed and where copies remain.

## Negative Consequences & Trade-offs

Maintaining indexes and tombstones adds storage and write cost, and a strict deletion cascade is more work than deleting a blob. The honest deletion report may disappoint users who expect "delete = gone everywhere", but it prevents a false guarantee; backups and provider-received copies remain a documented boundary.

## Rejected Alternatives & Negative Knowledge

### A single `OffstreamStatus` for retrieval state

Conflating execution, capture, and availability makes "not in the window", "not yet complete", and "purged" indistinguishable. Split fields make each state explicit.

### Walking a mutated parent chain to recover folded coverage

Coverage guessed from a mutated chain is wrong after compaction. The checkpoint source manifest is the authority.

### Checking authorization only when a request is compiled

A valid snapshot can wait in a queue while its sources are deleted. A separate check followed by an unfenced send also races revocation. Order each delivery/handoff with revocation and invalidate dependent responses, so an immutable request is an audit record rather than perpetual permission.

### "Delete = physically erase everywhere"

Backups, exports, and provider-received requests are beyond local GC; claiming otherwise is dishonest. Report each boundary separately.

## Links

- Sibling decisions: [ADR-0275](0275-immutable-execution-facts-and-branch-local-context-views.md), [ADR-0276](0276-bounded-raw-artifact-capture-and-durable-publication.md), [ADR-0278](0278-compaction-as-a-view-commit.md)
- Related records: [ADR-0262](0262-demand-paged-epistemic-memory-and-unified-inspect-channel.md), [ADR-0264](0264-epistemic-stream-spooling-content-aware-projection-and-unified-inspect-architecture.md), [ADR-0187](0187-persistence-v2-incremental-append-and-blob-reference-ledger.md)
