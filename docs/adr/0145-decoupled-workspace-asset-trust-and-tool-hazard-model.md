# 0145. Decoupled Workspace Asset Trust and Tool Hazard Model

- **Status:** Superseded by ADR-0147
- **Date:** 2026-08-26
- **Supersedes:** ADR-0140 (Workspace Authority and Content-Bound Extension Trust)

## Context

ADR-0140 attempted to bundle workspace execution authority (`unknown`, `restricted`, `development`) with project extension trust. This introduced severe architectural alienation:

1. **False Coupling:** It conflated "do I trust this workspace's local configurations/scripts?" with "can the AI execute commands and edit files?".
2. **Double-gating & Deadlocks:** AI tool execution was checked twice (once by the Permission Handler, and again by low-level `ExecutionEnvironment` checking `WorkspaceExecutionProfile::Development`). Unconfigured workspaces failed preflight unnecessarily, blocking safe read-only interaction.
3. **Sandbox as Authority:** Physical sandboxing was treated as a global workspace execution profile rather than a specialized tool capability.

## Decision

### 1. Completely Decouple Workspace Trust from Runtime Tool Execution

`WorkspaceTrust` has exactly one responsibility: **governing whether project-supplied static assets and configurations are loaded into the runtime**.

- **Protected Assets:**
  - Project skills (`.muta/skills/`, `.agents/skills/`, `skills/`)
  - Project MCP definitions (`.muta/mcp.json`)
  - Project hooks (`.muta/config.toml` `[[hooks]]`)
  - Project instructions / Prompt injections (`AGENTS.md`, `.cursorrules`, `.windsurfrules`)
  - Project config overrides (`.muta/config.toml`, `.mutarc`)
- **Trust States:**
  - `Absent`: Workspace contains no project-level contributions.
  - `Quarantined`: Project contributions exist but have not been approved by the user.
  - `Trusted`: Exact SHA-256 digest of all contribution files is explicitly approved and persisted.
  - `Changed`: Content digest changed (e.g. after a git pull), automatically falling back to quarantine until re-approved.

### 2. Runtime Authority Solely Governed by Tool Hazard Model

AI runtime actions follow an identical execution pipeline across all workspaces, governed solely by the **Tool Hazard Model + Permission Handler**:

- Tools declare their `HazardLevel` (`FileWrite`, `CommandExecution`, `ProcessLifecycle`).
- `PermissionPolicy` evaluates approvals against `PermissionStore` across three lifespans:
  - `Once`: Single execution approval.
  - `Session`: In-memory approval for the duration of the session.
  - `Always`: Persisted workspace-level rule.
- Execution environments (`LocalExecutionEnvironment`) unconditionally execute approved commands without secondary `WorkspaceExecutionProfile` gating.

### 3. Sandbox as an Explicit Tool Primitive

Physical container isolation is encapsulated as a dedicated tool (`SandboxBashTool`), available to specific presets (e.g. Code Analyst) for contained execution without host access.

## Consequences

- Completely eliminates `WorkspaceExecutionProfile` (`unknown`/`restricted`/`development`) and preflight blockers.
- Clean separation of concerns: static repository asset trust vs dynamic AI tool execution safety.
- Clear, uncompromised single source of truth for runtime permissions (`PermissionStore`).
