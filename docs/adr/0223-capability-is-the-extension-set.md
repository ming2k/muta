# 0223. Capability is the admitted extension set (retire the AgentRole capability surface)

- **Status:** Proposed
- **Date:** 2026-09-10
- **Amends:** [ADR-0211](0211-agent-role-harness-facets-and-ephemeral-ast-code-intelligence.md) — the role's capability face is dissolved; the worker-station/staffing framing is retained.
- **Builds on:** [ADR-0041](0041-tool-capabilities-scope-and-override.md) (tool scope), [ADR-0044] (operation scope), [ADR-0220](0220-personas-persisted-named-principals.md) (personas).

## Context

`AgentRole` (`crates/muta-contracts/src/agent_preset.rs`) bundles an
`AgentIdentity` with a **capability surface**: `agent_selection`
(tools + variants), `operation_scope` (path/command boundary), runtime knobs,
a `WorkspaceRequirement`, and ambient facets. Two facts make this surface a
liability:

1. **It is already half-redundant for personas.** Persona assembly
   (`registry.rs::assemble_hosted`) does
   `let mut role = AgentRole::for_role(persona.role_id(), &persona_identity);
   role.identity = persona_identity;` — the role's identity/directive is
   discarded. Only the capability face survives.
2. **A closed enum cannot express user capability.** `AgentRoleId` is compiled
   in; a user who wants "web search but no `ask_user`" cannot express it
   without a code change.

Meanwhile `operation_scope` is a second, orthogonal declaration of what tools
may do, duplicating information that belongs to the tools themselves (the
existing `ToolSelection.variants` already selects a `run_command` variant) and
to the execution environment (the workspace binding bounds paths).

## Decision

### 1. Delete the capability-surface abstraction

Capability **is** the set of admitted extensions (tools + harness extensions,
ADR-0224) plus the execution environment. There is no `AgentRole`-style profile
type; nothing declares "what this agent can do" beyond the list of things
attached.

### 2. `operation_scope` dissolves into its real owners

- **Command allowlists** become properties of the tool/variant. `run_command`
  gains variants that carry their own admission (e.g. a workspace variant and
  an audit variant), replacing `OperationScope.commands`.
- **Filesystem boundaries** are the execution environment's job: a workspace
  binding (and its confinement) admits paths; a workspace-free environment
  admits none. `OperationScope.paths` is deleted.

### 3. The workspace requirement is derived, not declared

There is no `WorkspaceRequirement` field. A workspace is present or not by
virtue of the session's execution environment. Validation rejects a resolved
extension set that requires a workspace when none is bound (and vice versa).

### 4. Runtime knobs belong to the persona, not to a capability profile

`AgentRuntimeConfig` (hard-stop, doom guard, model-stdin) rides on the persona
(ADR-0225).

### 5. `/role` becomes extension-set switching

Switching a role becomes replacing the admitted extension set on the live
agent. The name moves to `/persona` under ADR-0225.

## Invariants & Behavioral Boundaries

1. **The admitted extension set is the only capability declaration.** No
   separate profile/preset object is consulted at dispatch time.
2. **No `OperationScope` and no `WorkspaceRequirement` type survives.** Their
   concerns are owned by tool variants and the execution environment.
3. **Resolve-time validation.** An extension set is validated against the
   execution environment before the agent is constructed: a
   workspace-requiring extension with no workspace, or an unknown extension id,
   is a hard error.
4. **Tool variants carry their own admission.** Any per-command restriction is
   expressed as a selectable variant, not an out-of-band scope.
5. **Facets are extensions** (ADR-0224); a role cannot smuggle capability
   through a side channel.

## Alternatives considered

- **Keep `AgentRole` as a named preset library of capability profiles.**
  Rejected. It preserves the closed-enum indirection and the second source of
  truth the user must reason about; presets become a data convenience (a named
  extension list) under ADR-0224/0225, not a type.
- **Keep `operation_scope` as hidden metadata on tools.** Rejected. Two
  encodings of one rule (variant vs scope) drift; the boundary belongs to the
  tool or the environment.
- **Keep `WorkspaceRequirement` as a declaration.** Rejected. It can
  contradict the execution environment; deriving it removes the contradiction.

## Consequences

- `ToolSelection`, the tool pool, and the execution environment become the
  single source of truth for capability.
- The agent dispatch funnel loses the `operation_scope` gate; command
  restriction is enforced where the command tool is admitted.
- Existing `AgentRoleId` variants become named extension sets (shipped
  personas) rather than capability profiles.
- `Agent::apply_profile` is replaced by "resolve an extension set onto the
  agent" (ADR-0224/0225).

## References

- ADR-0041 — tool capabilities, scope, and override.
- ADR-0044 — operation scope.
- ADR-0211 — agent roles and harness facets (amended).
- ADR-0220 — persistent personas.
- ADR-0224 — unified extension primitive.
- ADR-0225 — persona owns identity and capability.
