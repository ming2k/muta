# 0233. Non-blocking mutation syntax diagnostics

- Status: Accepted
- Date: 2026-09-11
- Scope: agent/filesystem tools, extension interception, tool feedback
- Deciders: Maintainer
- Consulted: Maintainer–assistant implementation discussion
- Informed: Agent and tool maintainers
- Amends: [ADR-0214](0214-on-demand-code-structure-context-and-mutation-freshness.md), syntax rejection in the mutation lifecycle only

## Context and Problem Statement

Rejecting an entire candidate file prevents local repairs when unrelated syntax is already broken and prevents multi-step changes with intentionally incomplete intermediate states. Syntax parsers also cannot prove semantic correctness. Both tools and the code-intelligence extension previously exposed rejection paths, so changing only the tool body would leave contradictory policy.

This decision crosses the tool/extension/output boundaries and establishes binding mutation and safety invariants. The maintainer explicitly authorized replacing the hard syntax gate, not adding a bypass parameter.

## Decision Drivers

- Permit incremental repairs and invalid-to-valid editing sequences.
- Preserve actionable parser feedback and truthful write outcomes.
- Keep permissions, path confinement, unique matching, and filesystem errors authoritative.

## Considered Options

1. Keep rejection by default with a skip parameter.
2. Reject only newly introduced syntax errors.
3. Return non-blocking diagnostics after successful writes.
4. Remove syntax analysis entirely.

## Decision Outcome

Choose option 3. `edit_text` and `write_file` commit through the existing execution-environment filesystem first. Only successful writes produce mutation feedback with syntax diagnostics. JSON/TOML parsers and supported-language Tree-sitter checks remain available. The code-intelligence extension declares no interception hook and its compatibility entry points cannot block a mutation.

All successful mutations remain `ToolOutput::Patch`, including those with diagnostics. An additive `warnings: Vec<String>` field defaults to empty when deserializing older sessions and is omitted when empty. Warnings explicitly state that the write succeeded and remains applied and direct subsequent repair. They never contaminate `old`/`new` source fields. `to_text` includes warnings for the model and legacy callers; the UI renders them alongside the original rich diff. Patch identity, line provenance, and mutation freshness semantics remain intact.

### Invariants & Behavioral Boundaries

- Syntax errors never reject `edit_text` or `write_file`, including already-broken input, newly broken input, and file creation/overwrite.
- No skip parameter, opt-in recovery mode, or default syntax gate is permitted.
- Permissions, path rules, unique-match checks, argument validation, and filesystem failures remain hard errors.
- A failed write must not return successful mutation feedback. Uncertain filesystem failure does not prove rollback.
- Unsupported formats are not claimed to have passed syntax validation. Diagnostics are not compilation or type checking.
- ADR-0214's freshness, source provenance, and on-demand context requirements remain unchanged. This amendment replaces only its pre-write syntax rejection policy and associated recovery restriction.

### Positive Consequences

Incremental editing works without losing parser diagnostics or weakening file-access boundaries. One post-write helper owns feedback for both tools.

### Negative Consequences & Trade-offs

Invalid files can remain on disk and downstream consumers may fail until repair. Structured consumers must support the additive warnings field without treating it as failure or discarding the diff. Parser work still incurs latency and can produce false positives, but these are advisory. No semantic correctness guarantee is introduced.

## Rejected Alternatives & Negative Knowledge

- **Default gate plus skip parameter:** retains the broken workflow by default and makes ordinary repair dependent on a bypass.
- **Compare old and new error counts:** parser errors are unstable across edits; fewer errors do not prove improvement and unrelated failures still complicate repair.
- **Remove all analysis:** loses useful diagnostics unnecessarily.
- **Tool-only relaxation:** leaves an extension veto capable of restoring the rejected policy.

## Links

- [Filesystem tool reference](../reference/tools/filesystem.md)
- [ADR-0224: Extension primitive](0224-unified-extension-primitive.md) (proposed extension architecture; its historical syntax-gating context is not current policy)
