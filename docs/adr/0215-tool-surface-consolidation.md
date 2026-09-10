# 0215. Tool surface consolidation: one tool per resource lifecycle

- **Status:** Proposed
- **Date:** 2026-09-10

## Context

The developer master (`ToolScope::All`) exposes every registered tool each
round. Among them are two near-synonym families that inflate the model-visible
schema and add per-call decision points without adding capability:

**Background-job quartet.** `process_poll`, `process_logs`, `process_kill`,
and `process_wait` (`crates/muta-agent/src/tools/process_jobs.rs`) all operate
on the same `job_id` after `run_command` has spawned the job. Since ADR-0190's
two-phase tool protocol, `run_command` already returns the job id with an
automatic completion notification, which makes `process_poll` nearly redundant
(`status` is also available via `process_logs` with `tail_lines: 1`). The four
tools share one input (`job_id`), one service handle
(`BackgroundJobService`), and one state machine (`JobState`); they differ only
in which method of that service they call. ADR-0143 already established the
precedent: tools split by implementation convenience rather than by operation
kind create overlap and drift.

**Todo pair.** `write_todos` (full-replace via `TodoList::reconcile`) and
`update_todo` (surgical status edit by position or content substring) share
the same `TodoToolContext`, the same `TodoList` state, and the same render
output; they differ only in argument granularity
(`crates/muta-agent/src/tools/todo.rs`).

**A latent defect proves the drift is real.** `update_todo` is declared in
`AgentRoleDelegation::CODE_ANALYST_TOOLS` and the `reviewer`/`security`
`ToolSelection::only` lists (`crates/muta-contracts/src/agent_preset.rs:369,
454, 489`) and in the subagent tool policy
(`crates/muta-contracts/src/subagent.rs:339`) — but the installer
(`tool_integration.rs::install_agent_owned_tools`) upserts only
`TodoWriteTool`. Those roles advertise a tool the pool never installed. Two
parallel lists (scope declarations vs. actual registrations) drifted on the
first opportunity.

## Decision

One model-visible tool per resource lifecycle:

1. **`process`** replaces `process_poll` / `process_logs` / `process_kill` /
   `process_wait`:

   ```json
   {
     "job_id": "abc123",
     "action": "status | logs | wait | kill",
     "tail_lines": 50,
     "timeout_seconds": 60
   }
   ```

   `status` returns the `BackgroundJobInfo` snapshot (former poll, renamed to
   state what it returns); `logs` tails stdout/stderr (`tail_lines`, default
   50, max 200); `wait` blocks until a terminal state (`timeout_seconds`,
   default 60, max 600) and returns the tail; `kill` terminates the job. All
   four arms call the same `BackgroundJobService`; only the schema and the
   dispatch change. `tail_lines` and `timeout_seconds` are ignored outside
   their action.

2. **`todo`** replaces `write_todos` / `update_todo`, with mutually exclusive
   parameters:

   ```json
   { "items": [ { "content": "...", "status": "pending" } ] }   // full replace
   { "key": "2", "status": "completed" }                        // surgical update
   ```

   `items` present → full-replace reconcile semantics (identity preserved,
   exactly as today). `key` + `status` present → surgical update (position or
   case-insensitive substring match, exactly as today). Both or neither → a
   guiding error naming the two modes. The token-saving motivation of
   `update_todo` survives intact: a progress mark never re-sends the list.

   The name `todo` is pre-blessed: the `emit_todos_change` whitelist
   (`crates/muta-agent/src/agent/execution.rs:210`) already matches `"todo"`,
   so the TUI sticky-panel event fires without harness changes.

3. **Retirement deletes** (ADR-0135): the four old process names and the two
   old todo names leave no aliases, no teaching errors, and no scope-list
   presence. The `emit_todos_change` whitelist trims to the canonical names.

4. **Teaching copy migrates in lockstep** — the model learns tool names from
   spawn-success messages: `run_command`'s background/service replies
   (`execute_command/mod.rs:280, 354`), the shared detach notice in
   `muta-contracts/src/tool_output.rs:801`, and the mailbox wake hint in
   `muta-runtime/src/task_mailbox.rs:99` all reference `process`.

5. **Scope lists migrate in the same commit**: `CODE_ANALYST_TOOLS`, the
   `reviewer`/`security` selections, and the subagent tool policy switch
   `write_todos`/`update_todo` → `todo`. `process` needs no preset change
   (the developer master is `ToolScope::All`; the code analyst intentionally
   does not own background jobs).

### Invariants & behavioral boundaries

- **One tool per resource lifecycle.** Tools that operate on the same state
  object, with the same input key and the same result type, must be one tool
  with an action/mode discriminant — not siblings split by verb. Tools on
  *different* resources or different phases stay separate (ADR-0143's
  boundary: `run_command` dispatches; `process` intervenes; they do not
  merge).
- **No ghost tools.** Every tool name in any `ToolScope::Only` list, preset
  declaration, or subagent policy must be reachable through an actual
  registration; a scope list is dead if the installer never upserts the tool.
  Preset lists and registrations are updated in the same commit.
- **Retired tool names leave no residue**: no alias factory, no shim, no
  teaching error referencing a retired name (ADR-0135).

## Alternatives considered

- **Fold the process actions into `run_command` as parameters.** Rejected:
  `run_command` is the *dispatch* phase of a job; poll/logs/wait/kill are
  *post-spawn interventions* on an existing job. Merging phases yields a god
  tool whose meaning depends on mutually exclusive optional fields — the
  exact shape ADR-0143 rejected for the search surface.
- **Keep the four process tools.** Rejected: four schemas injected every
  round for one resource, with `process_poll` near-redundant given the
  ADR-0190 auto-notification. The residual cost is pure decision burden.
- **Keep the todo pair, only fix the missing `update_todo` installation.**
  Rejected: it repairs the symptom, not the decision-burden cause; the
  two-list drift class that produced the ghost tool remains available for
  the next rename.
- **Alias shims for the old names.** Rejected per ADR-0135 and ADR-0143's
  precedent: aliases preserve stale vocabulary in policies, prompts, and
  tests; the old names have no external consumers (unlike MCP tools).
- **Remove `ask_user` from the developer master.** Deliberately out of
  scope: in attended sessions it is the only structured channel for stopping
  at an ambiguity; stripping it belongs to the profile layer (unattended /
  `InteractionConfig` / `ToolScope`), not to tool deletion. Status quo
  affirmed by product decision 2026-09-10.

## Consequences

- Developer master tool count drops by four (two process tools + two todo
  tools → one each); every round's tool-schema injection shrinks
  accordingly (measurable via ADR-0188's content-addressed schema weights).
- The model faces one action-discriminant tool per family instead of two
  sibling pairs, removing the "which of these two do I call" decision at
  each call site.
- Stored prompts, external policies, and tests naming the retired tools must
  migrate (`muta-agent/src/tests.rs` shadow tool, `tool_call.rs` fixtures;
  the Google-protocol `write_todos` fixture is data, not policy, and may
  stay). Same migration class as ADR-0143's `glob`/`find`/`grep` retirement.
- `process_jobs.rs` collapses from four tool structs + four factories to
  one of each; `todo.rs` from two structs to one; both keep their existing
  service/state wiring unchanged.
- The `status` action is retained deliberately: `wait` can block up to
  600 s, so a cheap state probe retains independent value even with
  auto-notifications.

## References

- [ADR-0143: Filesystem search tools have one job each](0143-filesystem-search-tool-boundaries.md) — the direct precedent for one-tool-per-operation; its "one search tool with modes" rejection is why `run_command` does not absorb `process`.
- [ADR-0135: Retirement deletes](0135-retirement-deletes-no-teaching-errors.md) — no aliases for the six retired names.
- [ADR-0190: The agent is an actor — unified task fabric](0190-agent-as-actor-unified-task-fabric.md) — two-phase tool protocol and auto-notification, which demotes polling.
- [ADR-0144: Three-tier agent hierarchy and tool pool](0144-three-tier-agent-hierarchy-and-tool-pool.md) — `ToolScope::All` vs. declared lists.
- [ADR-0020: Unified task list](0020-unified-task-list.md) — the shared `TodoList` both todo modes mutate.
- [ADR-0179: Tool modality orthogonality and parameter ergonomics](0179-tool-modality-orthogonality-pattern-consistency-and-parameter-ergonomics.md) — action-discriminant naming and actionable diagnostics for mode errors.
- [ADR-0041: Tool capabilities, scope, and override](0041-tool-capabilities-scope-and-override.md) — the scope axis the ghost-tool defect violated.
