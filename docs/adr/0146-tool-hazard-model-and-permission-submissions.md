# 0146. Tool Hazard Model, Self-Submitting Payloads, and Permission Lifecycle

- **Status:** Accepted
- **Date:** 2026-08-26
- **Context & Predecessors:** ADR-0144, ADR-0145

## Context

Previous permission management conflated tool inspection, command interception, and user authorization into ad-hoc string comparisons and implicit gates. This created several failure modes:
1. **Unclear Threat Modeling**: Tools did not declare what kind of danger they represented (e.g. read-only vs file mutation vs arbitrary subprocess creation).
2. **Missing Process Intercept Metadata**: For command execution tools (`bash`), the system lacked structured descriptors for how the resulting process could be killed or reaped (e.g. process groups, `pkill` targets) during emergencies or cancellation.
3. **Inconsistent Authorizations**: Approval scopes were ambiguous between single-turn grants, session-wide grants, and permanent workspace allowlists.

## Decision

### 1. Tool-Centric Threat Model (`HazardLevel`)

Every tool in the unified `ToolPool` explicitly declares its `HazardLevel`:
- `HazardLevel::Safe`: Read-only inspections (`read_file`, `find_files`, `search_text`, `list_dir`, `websearch`). Bypasses permission prompting entirely.
- `HazardLevel::FileModification`: In-place file mutations and overwrites (`write_file`, `edit_file`). Requires permission evaluation.
- `HazardLevel::CommandExecution`: Arbitrary OS process execution (`bash`, native shell). Requires permission evaluation.
- `HazardLevel::ProcessLifecycle`: Task and process manipulation (`kill`, `cancel`). Requires permission evaluation.
- `HazardLevel::NetworkOrExternal`: External network or MCP tool calls.

### 2. Autonomous Tool Submission Protocol (`ToolPermissionSubmission`)

Hazardous tools must autonomously build and submit a structured description of their intent to the `PermissionHandler` before execution:
- **File Tools** submit `ToolPermissionPayload::FileEdit`:
  - Target file paths being modified.
  - Operation type (`edit_file`, `write_file`).
- **Command Tools** submit `ToolPermissionPayload::Command`:
  - The exact command line string.
  - Working directory (`cwd`).
  - `ProcessKillSpec`: Process termination specification, including process group killability (`killpg`) and standard `pkill` target patterns.

### 3. Clear Three-Tier Permission Lifespan

The `PermissionHandler` maps every dangerous tool submission to the active workspace with three distinct grant lifespans:
1. **`Once`**: Approves only the immediate single invocation. Not stored in memory or on disk.
2. **`Session`**: In-memory authorization valid for the duration of the current session. Cleared on session termination or daemon restart.
3. **`Always`**: Persistently stored in `data/permissions/<workspace_id>.json` bound to the workspace root. Survives restarts and automatically satisfies subsequent matching invocations.

## Consequences

- All tools self-describe their threat profile, enabling automated safe-path bypass without sacrificing security.
- Running processes have a standard `ProcessKillSpec` contract for clean task cancellation and emergency kill.
- Workspace-level permissions are cleanly partitioned into `Once`, `Session`, and `Always` without coupling to static extension trust.
