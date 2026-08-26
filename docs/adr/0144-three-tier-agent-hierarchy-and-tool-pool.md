# 0144. Three-tier Agent Hierarchy and Unified Tool Pool

- **Status:** Accepted
- **Date:** 2026-08-26
- **Supersedes:** ADR-0042 (Principal-Envoy Role Vocabulary)

## Context

The legacy architecture divided agents into two loosely categorized roles: `Principal` and `Envoy`. As Muta evolved into a multi-session daemon with cross-session coordination, background exploration, and diverse execution roles (Developer, Code Analyst, etc.), this two-tier model became insufficient:

1. Daemon-level operations (session coordination, health tracing, global debug traces) lacked a first-class agent identity.
2. The `Principal / Envoy` terminology drifted across subsystems, leading to inconsistent naming in constants (`ENVOY_DRAIN_GRACE`, `ENVOY_TOOL_DESCRIPTION`) and confusion with low-level task supervisors (`supervise.rs`).
3. Tools were registered and duplicated across ad-hoc sets, causing cache fragmentation and divergent dispatch paths.

## Decision

### 1. Establish the Three-tier Agent Model

Standardize all agents into a strict three-tier hierarchy:

- **`Supervisor` (Singleton Daemon Agent):** Exactly one instance per Muta service. Responsible for daemon-wide session orchestration, cross-session coordination, debug tracing, and global lifecycle management.
- **`Master` (Session Agent):** Exactly one active instance per session. Manages user dialogue, conversation state, task progression, and subordinate runners. Configured with distinct dynamic presets (e.g. Developer, Code Analyst).
- **`Runner` (Execution Sub-Agent):** Transient, bounded execution units spawned by Master agents to perform targeted sub-tasks (exploration, code editing, deep research).

### 2. Unified Global `ToolPool`

All tools are declared and housed within a centralized `ToolPool`. Agents declare their required tool dependencies from this pool:
- Avoids multiple independent tool registry mirrors.
- `ToolManager` acts as the single facade for tool resolution, dynamic MCP merging, and per-turn schema generation (`loop_tools`).

### 3. Clear Terminology Governance

- Fully retire all legacy `Principal / Envoy` vocabulary and constants.
- Rename low-level task panic supervisors to `task_fault_tolerance.rs`, preventing naming collisions with top-level `Supervisor`.
- Standardize configuration under `[master]` (e.g. `[master.doom_guard]`).

## Consequences

- Clean conceptual hierarchy where every entity is an `Agent` distinguished by tier, mission, and tool declaration.
- Deterministic KV-cache stability across turns through centralized tool resolution.
- Seamless multi-agent collaboration with unified distributed trace metadata (`MeshEnvelope`).
