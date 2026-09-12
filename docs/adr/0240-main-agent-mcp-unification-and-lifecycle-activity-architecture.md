# 0240. Main-Agent Direct MCP Unification and Lifecycle Activity Architecture

- **Status:** Proposed
- **Date:** 2026-09-12
- **Scope:** `muta-mcp`, `muta-agent` (tools admin, subagents), `muta-runtime` (bootstrap, file-watcher, trust gate), `mutx` (activity bar, chrome, notifications)
- **Supersedes:** ADR-0138 (Actor-Model Subagent Isolation and MCP Sandboxing)
- **Amends:** ADR-0060 (Skills and MCP Extension Boundaries), ADR-0085 (Config-time Tool Scoping), ADR-0110 (Activity Bar Scope), ADR-0154 (Activity Bar Breathing Anchor)
- **Related:** ADR-0137 (Server-Side KV Cache Alignment and Zoning), ADR-0144 (Three-Tier Agent Hierarchy), ADR-0147 (Orthogonal Workspace Security Planes)

---

## Context and Problem Statement

Model Context Protocol (MCP) tool integrations historically suffered from architectural compromises driven by prompt caching anxiety (ADR-0137 / Zone 1):
1. **The Subagent Indirection Anti-Pattern**: ADR-0138 attempted to isolate dynamic MCP tools within an ephemeral `mcp_specialist` subagent to protect the main conversation's 100% invariant Zone 1 KV cache. In practice, this broke developer ergonomics: the main agent lost direct access to external capabilities (databases, GitHub, issue trackers), multi-turn round-trips doubled token latency, and interactive conversational steering collapsed under nested child agent delegation.
2. **Asymmetric Configuration & Security Posture**: MCP configurations exist in two separate tiers — global user configuration (`~/.config/muta/config.toml`) and repository-scoped configuration (`.muta/config.toml`, `.muta/mcp.json`). Without strict, clean-break boundaries, security gates either over-prompted users on their own global tools or left workspaces open to unverified process execution.
3. **Black-Box Cold Starts in UI Chrome**: While `muta-mcp` connects servers concurrently in the background (ADR-0098), the TUI Activity Bar (`apps/terminal/crates/mutx/src/chrome/activity_bar.rs`) was strictly scoped to in-round turn execution (`round_active && status != "idle"`). During cold start, when background MCP connections are initializing (taking 500ms–2000ms), the activity bar collapsed to zero height. Users were left with a blank, frozen-looking screen without progress visibility.
4. **Manual Reconfiguration Friction**: Changes to user-level or workspace MCP configurations required manual `/reload` commands or modal toggles rather than reacting to kernel-level filesystem events.

We require an uncompromised, legacy-free architecture that restores direct MCP capability to the main agent, enforces clear security boundaries, reacts to live filesystem events, and upgrades the Activity Bar into a prioritized lifecycle telemetry surface.

---

## Decision Drivers

- **Zero-Indirection Ergonomics**: The principal agent must invoke any enabled MCP tool directly with native function calling and exact JSON schema enforcement.
- **Uncompromised Security Boundaries**: Global user configurations must be implicitly trusted, while workspace-provided executables must remain fail-closed behind mandatory trust verification.
- **Honest Cache Semantics**: Deliberate capability additions (user adds/enables an MCP) represent an intentional change of intent. The system must accept the single-turn cache recomputation without artificial freezing, delayed activation, or subagent workarounds.
- **Continuous System Visibility**: Cold-start initialization and hot-reloading must provide immediate, high-fidelity visual progress in the primary terminal chrome.
- **Kernel-Driven Reactivity**: File-system modifications to MCP declarations must automatically propagate without manual intervention.

---

## Considered Options

- **Option 1: Retain Subagent Quarantine (ADR-0138)**: Restrict MCP tools exclusively to child agents to preserve the main agent's Zone 1 KV cache.
- **Option 2: Dynamic In-Prompt Tool Gateway (`mcp_gateway`)**: Pass a generic meta-tool schema in Zone 1 and inject raw MCP schemas into Zone 3 (ephemeral tail).
- **Option 3: Main-Agent Direct Exposure + Two-Tier Trust + Multi-State Activity Bar (Chosen)**: Retire MCP subagents; expose MCP tools directly to the master agent; enforce implicit user trust vs. quarantined workspace trust; hook inotify config reloading; elevate the Activity Bar into a prioritized lifecycle and round status machine.

---

## Decision Outcome

Chosen option: **Option 3**.

We implement a clean-break architecture with zero legacy baggage across five core pillars:

```text
┌─────────────────────────────────────────────────────────────────────────────┐
│ 1. Principal-Only MCP Exposure                                              │
│    • Main Agent directly owns all enabled MCP tools in its active toolset.  │
│    • Subagent MCP binding (`mcp_specialist`, `delegate_mcp`) is retired.    │
└──────────────────────────────────────┬──────────────────────────────────────┘
                                       │
┌──────────────────────────────────────▼──────────────────────────────────────┐
│ 2. Two-Tier Authority & Security Model                                      │
│    • User Scope (`~/.config/muta/config.toml`): Implicitly trusted.        │
│    • Workspace Scope (`.muta/mcp.json`): Quarantined behind Trust Gate.    │
└──────────────────────────────────────┬──────────────────────────────────────┘
                                       │
┌──────────────────────────────────────▼──────────────────────────────────────┐
│ 3. Kernel File-System Reactivity                                            │
│    • Inotify/kqueue watcher monitors user and workspace config paths.       │
│    • User config edits trigger hot reconfigure + non-blocking toast.        │
│    • New workspace MCP configs trigger immediate Trust Gate consent banner. │
└──────────────────────────────────────┬──────────────────────────────────────┘
                                       │
┌──────────────────────────────────────▼──────────────────────────────────────┐
│ 4. Prioritized Lifecycle Activity Bar (`ActivityPriorityMachine`)            │
│    • Priority 1: Interactive Gates (Permission prompts, AskUser).           │
│    • Priority 2: Round Execution (Thinking, tool execution, model stream).   │
│    • Priority 3: System Lifecycle (MCP connecting progress, hot reloading). │
│    • Priority 4: Idle (Collapsed).                                          │
└─────────────────────────────────────────────────────────────────────────────┘
```

### 1. Principal-Only Direct Capability
- All enabled, trusted MCP tools are registered directly into the Master Agent's `ToolManager` and exposed through `visible_tools()` on every turn.
- The `mcp_specialist` subagent role, `delegate_mcp` tool alias, and `SubagentTool::bind_dynamic_tool_source` mechanism are eradicated.
- Subagents (`RUNNER_EXPLORE`, `RUNNER_CODE`) remain dedicated solely to disposable compute and sandbox tasks; they do not inherit MCP capabilities.

### 2. Dual-Scope Security & Trust Model
- **User Level (`~/.config/muta/config.toml`)**:
  - Maintained by the machine owner.
  - Automatically admitted and started on launch without interactive security gates.
- **Workspace Level (`.muta/config.toml`, `.muta/mcp.json`)**:
  - Cloned or imported from untrusted project repositories.
  - Stored in `WorkspaceSecurityStore` under `TrustDomain::Mcp`.
  - Stays strictly quarantined (`WorkspaceTrustState::Quarantined`) until the user explicitly issues `/trust mcp` or approves the security banner.

### 3. Inotify/Kernel-Driven Dynamic Hot-Update
- `muta-runtime` attaches an asynchronous filesystem watcher (`notify`) to:
  1. `$XDG_CONFIG_HOME/muta/config.toml`
  2. `<workspace_root>/.muta/config.toml`
  3. `<workspace_root>/.muta/mcp.json`
- Edits to the user-level config trigger a debounced (500ms) execution of `McpRuntime::reconfigure()`:
  - Added/updated servers are spawned and connected in parallel.
  - Removed servers are cleanly killed (`SIGTERM` -> `SIGKILL`).
  - Active sessions receive a non-blocking system toast notification via `RoundEvent::Notice`:
    `[mcp] Configuration reloaded (+postgres: 4 tools, -old_tool)`.
- If an untrusted workspace-level MCP file is added or modified, the runtime broadcasts a security review requirement, preventing process execution until approved.

### 4. Redesigned Activity Bar Lifecycle State Machine
`mutx` refactors `apps/terminal/crates/mutx/src/chrome/activity_bar.rs` and `apps/terminal/crates/mutx/src/phase.rs` from a turn-local renderer to a 4-tier prioritized state machine:

```rust
pub enum ActivityState<'a> {
    /// Priority 1: Interactive blocking gate (user intervention required).
    InteractiveGate {
        action: &'a str,
        target: &'a str,
    },
    /// Priority 2: In-turn active LLM reasoning or tool dispatch.
    RoundExecution {
        phase: &'a str,
        elapsed: Option<Duration>,
    },
    /// Priority 3: Daemon & subsystem bootstrapping / background lifecycle.
    SystemLifecycle {
        phase: SystemPhase,
        progress: (usize, usize),
        detail: &'a str,
    },
    /// Priority 4: All systems idle and healthy.
    Idle,
}
```

- **Bootstrapping / Cold-Start State**: When `mutx` attaches to a session whose MCP runtime has pending connection handshakes, the Activity Bar renders:
  `● Connecting MCP servers (1/3: github, postgres)... [0.4s]`
  with an animated breathing indicator.
- Once all MCP handshakes resolve (or reach the 8s timeout), the bar transitions to `Idle` (height collapses to 0) or immediately yields to incoming round activity.

### 5. Honest Cache Economics
- When a user deliberately adds an MCP server mid-session, the tool definitions are immediately added to Zone 1 on the subsequent turn.
- We acknowledge and accept the physical KV-cache recomputation on that single transition turn: user intent to change capabilities inherently introduces a new prompt prefix.
- The system prevents *unintentional* cache churn by:
  1. Lexicographically sorting all tool schemas deterministically.
  2. Freezing existing tool definitions against transient network/transport reconnections (a reconnected server re-binds without mutating its schema).

---

### Invariants & Behavioral Boundaries

- **`[INV-MCP-01]` Master-Only Affinity**: MCP tools are registered strictly with the Master Agent. Subagents must never be injected with dynamic MCP tool handles.
- **`[INV-MCP-02]` Fail-Closed Workspace Gate**: No workspace-declared MCP executable (`.muta/mcp.json` or `.muta/config.toml`) may spawn or execute until `TrustDomain::Mcp` is `Trusted`.
- **`[INV-MCP-03]` Immediate User-Level Admission**: User-level MCP configurations (`~/.config/muta/config.toml`) are admitted immediately without trust gate prompts.
- **`[INV-MCP-04]` Lifecycle Activity Visibility**: Any asynchronous system state that defers capability readiness (MCP bootstrapping, catalog reload) MUST be projected through the Activity Bar if no higher-priority interaction is active.
- **`[INV-MCP-05]` Output Compaction Boundary**: MCP tool outputs exceeding 16,384 bytes or 200 lines must be spilled to a temporary file in `$TMPDIR`, returning a structured head/tail summary and URI path to prevent transcript poisoning.

---

## Positive Consequences

- **Superior Developer Ergonomics**: Natural, zero-overhead direct tool calls for all MCP capabilities.
- **Zero Startup Confusion**: Cold-start MCP connection latency is immediately obvious via the Activity Bar progress indicator.
- **Real-Time Hot Reloading**: Edits to configuration files take effect immediately across running sessions without daemon restarts or manual `/reload` commands.
- **Codebase Simplification**: Eliminates complex cross-agent capability leasing, dynamic sink cloning in subagents, and profile hacking.

---

## Negative Consequences & Trade-offs

- **Mid-Session Cache Invalidation on Mutation**: Adding or enabling an MCP server during an ongoing conversation incurs a one-turn KV-cache recomputation cost.
  - *Mitigation*: This occurs exclusively on deliberate user modification, never during passive execution.
- **Activity Bar Layout Churn**: Cold-start activity rendering expands the activity bar by 1 row during initial connection, then collapses it on completion.
  - *Mitigation*: The composer input box remains anchored flush; the activity bar animates smoothly with the standard breathing glyph.

---

## Rejected Alternatives & Negative Knowledge

### Retaining Subagent-Only MCP (`mcp_specialist`)
- **Why Considered**: Protects the main agent's Zone 1 KV cache unconditionally.
- **Why Rejected**: Degrades conversational UX. The main agent cannot reason over direct tool results, latency doubles, and multi-step interactive commands fail frequently due to context loss in child agents.

### Universal Generic Gateway Tool (`mcp_gateway`)
- **Why Considered**: Employs a single `call(server, tool, args)` schema in Zone 1, moving tool definitions to runtime prompt text.
- **Why Rejected**: Disables native provider grammar constraints (Constrained Decoding). Frontier models hallucinate parameter names, pass wrong data types, and miss required arguments.

### Artificial Tool Freezing on Session Start
- **Why Considered**: Ignores mid-session configuration additions until a new session is spawned to maintain cache hit statistics.
- **Why Rejected**: Violates the principle of least surprise. A developer adding an MCP integration expects it to be available immediately.

---

## Links

- Supersedes: `docs/adr/archive/0138-actor-model-subagent-isolation-and-mcp-sandboxing.md`
- Implements: `crates/muta-mcp`, `crates/muta-agent`, `apps/terminal/crates/mutx`
- Invariants: `[INV-MCP-01]` through `[INV-MCP-05]`
