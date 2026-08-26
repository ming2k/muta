# 0147. Orthogonal Workspace Security Planes

- **Status:** Accepted
- **Date:** 2026-08-26
- **Supersedes:** ADR-0145; ADR-0142's project-local root-admission source
- **Revises:** ADR-0146

## Context

The workspace security implementation accumulated several partially
overlapping meanings of trust. Asset admission, filesystem containment, and
runtime authorization were represented by aggregate workspace states and
several command paths. This produced contradictory guidance such as asking an
operator to run `/trust workspace` even though that subcommand was no longer
accepted.

The three decisions answer different questions and must not imply one another:

1. May project-authored configuration and prompt assets be loaded?
2. Which physical directories may file tools address?
3. May this concrete hazardous operation execute now?

## Decision

### 1. Project asset trust is content-bound and domain-specific

Asset trust contains only concrete domains:

- `mcp`: `.muta/mcp.json` and the `[mcp]` projection of
  `.muta/config.toml`;
- `skills`: `.muta/skills`, `.agents/skills`, `.claude/skills`, and
  `skills`;
- `hooks`: `.muta/hooks` and the `[[hooks]]` projection of
  `.muta/config.toml`;
- `rules`: project instructions and project slash commands.

Each present domain is attested independently with a SHA-256 digest over its
paths, file bytes, and relevant file modes. Grants are stored by canonical
workspace root in `workspace_security.json`. A content change changes only
that domain to `changed`; it cannot invalidate or authorize another domain.
Symlinked assets fail closed.

`all` is a command-layer selection, not a persisted domain. The canonical
grammar is:

- `/trust` and `/trust all`: trust every present domain;
- `/trust mcp`: trust only MCP definitions;
- `/trust skills`: trust only project skills;
- `/trust status`: report all concrete domain states;
- `/trust revoke` and `/untrust`: revoke every domain grant.

There is no `/trust workspace`, `/trust extensions`, or `/extensions`
compatibility path. A trust mutation has one live-apply path that reloads or
unloads MCP, skills, hooks, rules, and project commands from one fresh
snapshot.

### 2. Filesystem containment is user-owned spatial policy

Native file operations are confined to the canonical primary workspace root
plus explicitly configured linked roots. Every read, write, edit, directory
operation, and metadata lookup resolves the existing ancestor of the target,
canonicalizes it, and checks it against the admitted root set before I/O.
Dangling links and path-resolution failures are denied.

Linked roots come from the user-owned global `[workspace].additional_roots`
configuration. Relative entries resolve from the active project root. A
repository's `.muta/config.toml` is never consulted for root admission: project
content cannot widen the boundary that contains that project. Invalid root
configuration fails closed to the primary workspace.

Asset trust neither widens nor narrows the root set.

### 3. Runtime authorization is operation- and hazard-specific

Every hazardous tool invocation supplies a `HazardLevel`, exact scope, and
structured permission payload. File modifications, both shell variants, MCP
tool calls, process lifecycle operations, and command hooks are evaluated by
runtime permission policy. Grants retain their distinct lifetimes:

- `Once`: the pending invocation only;
- `Session`: the current in-memory session;
- `Always`: a durable exact rule for the workspace.

Autopilot changes only interaction. It executes an already-authorized call and
fails a missing grant immediately; it never manufactures authority. Missing
authority reports `[permission required]` and directs the operator to approve
interactively or add a permission rule. Asset-trust commands are never
suggested as a runtime-permission remedy.

Command hooks run inside agent control flow, where recursively opening a
permission prompt would be unsafe. They therefore require an already-present
exact `tool = "hook"` permission rule and otherwise skip with a fail-closed
diagnostic.

## Consequences

- A repository can be trusted as prompt/configuration input without receiving
  filesystem or execution authority.
- Linked repositories can be edited without trusting their project assets.
- Runtime grants do not cause MCP, skills, hooks, or rules to load.
- Removing aggregate trust fields and retired command aliases is a deliberate
  schema and command-surface break. Version-1 aggregate trust cannot be mapped
  safely to independent domains and is discarded only when the operator makes
  a new explicit trust mutation.
- Project MCP and hook processes remain read-only and offline when loaded;
  their individual MCP calls or hook executions still follow the runtime
  policy described above.

