# 0214. On-Demand Code Structure Context and Mutation Freshness

- Status: Accepted
- Date: 2026-09-10
- Scope: agent/code intelligence, filesystem tools, context producers
- Deciders: Maintainer
- Consulted: Maintainer–assistant design discussion
- Informed: Agent and tool maintainers
- Related RFC: None; proposal originates from the context-zoning design discussion
- Depends on: [ADR-0213](0213-model-request-composition-and-context-lifecycle.md)
- Revises proposed direction: [ADR-0211](0211-agent-role-harness-facets-and-ephemeral-ast-code-intelligence.md), code structure delivery and mutation support only

## Context and Problem Statement

Automatically appending a repository outline to every model request gives immediate navigation at the cost of repeated scanning, temporary-tail cache discontinuity, and potentially irrelevant context. Retaining every outline version in history avoids one source of sequence discontinuity but accumulates stale evidence and consumes the context window. Cache hits do not remove those tokens from the model's context or prevent confusion between versions.

The current workspace already contains relevant capabilities:

| Capability | Current implementation boundary |
| :--- | :--- |
| `get_outline(path)` | `crates/muta-agent/src/tools/get_outline.rs` reads a file and extracts a structural outline |
| Syntax checking | `edit_text` and `write_file` call `syntax_guard::verify_syntax` before writing |
| Structure extraction | `crates/muta-agent/src/syntax/mod.rs` reparses content; a full incremental index and AST delta pipeline are not established capabilities |
| Automatic repo map | `CodeIntelligenceFacet` projects a repository summary during request construction |
| Facet mutation hooks | The contract exposes mutation interception, but the inspected production write paths call the syntax guard directly; a declared hook is not evidence of an integrated lifecycle |

These are observations motivating migration, not guarantees that all role, filesystem, or mutation paths already share one policy.

This decision meets the significance threshold by changing boundaries among context assembly, tools, file access, and derived state, and by establishing enforceable freshness and output-budget rules.

## Decision Drivers

- Prefer relevant current evidence over automatic repository-wide context.
- Avoid repetitive snapshots, conflicting historical versions, and context-window pressure.
- Reuse existing outline and syntax-checking capabilities.
- Keep correctness independent of whether a parser cache exists.
- Cover mutations outside built-in editing tools without claiming perfect filesystem interception.

## Considered Options

1. Automatically regenerate a repository map in every temporary tail.
2. Append each new AST or outline version to model history.
3. Query bounded code structure on demand and manage freshness outside the conversation.
4. Build a persistent global semantic index before changing context delivery.

## Decision Outcome

Choose option 3. The target architecture is:

```text
File changes -> invalidate or refresh derived structure outside model context
Task needs structure -> authorized tool query -> bounded, versioned evidence
Successful edit/write -> mutation outcome -> invalidate affected derived state
Next model request -> assemble prepared context; do not scan the repository
```

### On-Demand Structure Delivery

Use the existing `get_outline` capability for local navigation, followed by source reading when implementation details matter. Do not inject a generic repo map on every request by default. A future repository-level query may provide explicit scope and budget without restoring ambient full-map injection.

An outline is a syntactic summary, not a complete AST, type analysis, dependency proof, or behavioral specification. Tool descriptions and results must not represent it as semantic ground truth. Source contents and their summaries remain untrusted task data rather than instructions.

A returned tool result is history-bearing evidence under ADR-0213. It is not retroactively ephemeral because its source later changes. Re-query only when task relevance and freshness require it; remove obsolete detail through explicit history compaction rather than rewriting old tool results.

### Mutation Lifecycle

Keep three responsibilities separate:

1. **Before write:** validate the candidate content under the existing supported-format syntax policy and file access rules. A rejected candidate must not be written. Unsupported formats must not be reported as successfully syntax-validated. Syntax checking is not type checking, compilation, or a guarantee of a correct edit.
2. **After successful write:** make cached or indexed structure for the changed file ineligible for reuse until its source version is validated or rebuilt. Publish refreshed structure only for bytes actually committed, not merely for the proposed candidate.
3. **Tool feedback:** return the mutation outcome and actionable diagnostics. Do not automatically attach the complete new outline or AST. Any optional structural delta must be explicitly selected, relevant, and bounded.

A rejected candidate must not publish a new source version. If a filesystem operation fails with an uncertain or partial outcome, conservatively mark derived state stale and re-read; an error is not proof that disk contents stayed unchanged. If post-write indexing fails, keep the write outcome truthful and the derived state invalid rather than misreporting a completed write as rolled back.

One production mutation lifecycle must own validation and invalidation. Tools and facets may participate through that lifecycle, but must not maintain independent, contradictory implementations. The concrete hook API is an implementation choice, not a new parallel architecture prescribed here.

### Freshness Without Mandatory Index Infrastructure

Parsing the authorized current file on demand is a valid initial implementation. This ADR does not require a persistent cache, background watcher, incremental parser, or LSP service.

If caching is introduced, entries must identify the workspace/environment, canonical file identity, content version, and relevant parser configuration. Returned evidence must identify the source version it describes. Metadata-only checks must not claim content identity when they can miss changes; use a content check or another mechanism with an explicit equivalent guarantee.

Built-in edit/write notifications are an optimization, not the complete freshness authority. Queries must account for shell writes, external edits, deletion, rename, and environment/workspace changes. A watcher may assist invalidation, but the query path must not silently serve known-stale data when events are missed.

A query describes the bytes read at its identified snapshot. Concurrent changes afterward remain possible: do not promise a permanently current result. Editing must continue to use its own existing conflict and permission checks; an outline is not authorization to overwrite a newer file.

### Small Optional Change Reminders

An external change affecting evidence the active task is using may justify a short request-local reminder to re-read a named file. It does not justify automatically sending a replacement full repository map. A model's own successful edit normally needs no additional ambient reminder because the tool outcome already reports the mutation.

Reminder producers must satisfy ADR-0213's relevance, finite-budget, prepared-data, and retry rules. If relevant change information is unavailable, do not perform a hidden scan during request assembly to manufacture it. Index freshness must not depend on whether the model received or followed a reminder.

### Invariants & Behavioral Boundaries

The following become binding upon acceptance:

1. Default request construction must not scan, parse, or automatically attach a repository-wide outline. Code structure enters through authorized, scoped tool retrieval; optional change reminders are not a map-delivery back door.
2. Queries must use the applicable execution-environment filesystem and permission boundary. Index reuse must not expose files across workspaces, environments, or revoked access boundaries.
3. Structure results must have finite output limits and disclose truncation and unsupported or unavailable analysis. File count, input size, and execution work must also be bounded; output clipping alone is not a resource policy.
4. Every returned structural result must identify its source and version. A known-stale entry must be refreshed or reported unavailable, not labeled current. No full index is required to satisfy this rule.
5. Successful built-in mutations must invalidate affected derived state before subsequent queries can treat it as current. Failed or uncertain operations must follow the lifecycle above.
6. Non-tool mutations must be detected or validated at retrieval; an edit/write-only event stream is insufficient as the freshness authority.
7. File changes must not automatically append structural snapshots to history. Historical tool results retain their original provenance until explicit compaction or reconstruction.
8. Syntax validation, freshness maintenance, and context delivery must remain separate responsibilities. No millisecond latency, complete semantic validity, or unconditional cache-hit claim follows from using Tree-sitter.
9. Any parser or index resources must have explicit ownership, isolation, bounds, and teardown. This proposal does not authorize an unbounded global singleton or mandate a daemon indexing service.

### Positive Consequences

- Structure information is selected for the task instead of automatically competing with it.
- Frequent file changes do not imply frequent context injections.
- Existing tools can support the first implementation without a large indexing subsystem.
- A single freshness contract covers built-in and externally initiated changes.

### Negative Consequences & Trade-offs

- Initial navigation may require another model/tool round trip; bounded targeted retrieval trades that latency for lower routine context volume.
- Tool evidence still occupies history and can become stale. Version provenance, re-reading, and explicit compaction mitigate rather than eliminate this risk.
- Reading or hashing source bytes to validate cache identity has a cost. Caching strategies should be selected from measured workloads, not unsupported performance promises.
- Syntax gates can reject work in already malformed files. This ADR preserves the existing candidate-validation policy; recovery modes or a new error-tolerance policy require separate review rather than being introduced implicitly.

## Rejected Alternatives & Negative Knowledge

### Per-Request Ambient Repository Map

It provides zero-tool-call orientation, but pays scanning and context costs even for unrelated tasks and changes the sequence before prior generated output. Moving the map to the tail does not guarantee complete cross-request cache retention.

### Append Every AST Version to History

It can preserve sequence continuity but accumulates conflicting snapshots and consumes the context window. A newer-version notice does not guarantee that the model ignores obsolete symbols. A high cache-hit rate is not a measure of evidence quality.

### Automatically Return the Full AST After Each Write

It transfers the same overload from the temporary tail into durable tool history and duplicates information often already apparent from the edit. Small optional deltas are not permission for unbounded structure emission.

### Trust Only Built-In Mutation Notifications

Shell commands, external editors, rename, and deletion bypass those notifications. A cache can be internally consistent with its event stream and still disagree with source bytes.

### Require a Persistent Global Semantic Index First

It adds ownership, concurrency, lifecycle, and migration decisions before demand is established. Current-file parsing is a valid correctness baseline. A substantial future indexing service should receive its own architectural review if justified by measurements.

## Relationship to Existing Decisions

ADR-0213 owns generic request and history semantics; this proposal applies them to code structure without redefining the zones.

ADR-0211 is still Proposed, not an accepted decision being silently superseded. If this proposal is accepted, its on-demand delivery direction replaces ADR-0211's proposed automatic Zone 3 repo map, rejection of tool-first structure retrieval, and automatic AST-delta intake direction. It also replaces unqualified incremental-parsing latency guarantees with the explicit validation and freshness contract above. It does not decide the `AgentRole`/`HarnessTask` terminology or the general existence of facets. Maintainer review must reconcile those scoped overlaps before accepting incompatible proposals; this change does not alter ADR-0211's status.

## Verification and Adoption

The following are implementation acceptance criteria, not tests claimed to pass in this documentation-only change:

- Ordinary model requests do not trigger repository traversal or contain an automatic repo map, including default agents, role changes, and subagents.
- Explicit outline retrieval returns authorized, scoped evidence with source-version identity and bounded output; truncation and unsupported analysis are visible.
- Successful edit/write operations make old derived state ineligible; rejected candidates do not publish new versions. Partial-write or uncertain failures force revalidation.
- Shell/external edits, rename, deletion, and same-size content replacement cannot silently reuse stale structure as current.
- Concurrent changes yield evidence labeled for the actual snapshot read; stale evidence cannot bypass edit conflict checks.
- Post-write index failure does not conceal a successful mutation or expose a stale entry as fresh.
- Tool results remain historical evidence; repeated file changes alone add no full AST snapshots to conversation history.
- Permission denial and workspace/environment switching cannot leak cached file content.
- Optional change reminders obey relevance and budget limits, and do not rebuild repository structure during assembly.

Implement in small changes: first remove default map projection while preserving tools; then unify mutation lifecycle and version provenance; introduce caching only if measurements justify it. Update living context explanations and affected tool reference documentation alongside the corresponding code changes. Do not describe these proposal targets as already deployed.

## Links

- [ADR-0213: Model request composition and context lifecycle](0213-model-request-composition-and-context-lifecycle.md)
- [ADR-0211: Agent roles, harness facets, and ephemeral AST code intelligence](0211-agent-role-harness-facets-and-ephemeral-ast-code-intelligence.md)
- [ADR-0137: Server-side KV-cache alignment and zoning](0137-server-side-kv-cache-alignment-and-zoning.md)
