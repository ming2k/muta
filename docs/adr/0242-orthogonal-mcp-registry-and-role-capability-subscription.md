# 0242. Orthogonal MCP Architecture: Environment Server Registry, Role Capability Subscription, and Dynamic Session Masking

- **Status:** Accepted
- **Date:** 2026-10-02
- **Scope:** `core/contracts`, `core/agent`, `muta-mcp`, `muta-persistence`, `muta-runtime`
- **Deciders:** Muta Architecture Team
- **Builds on:** [ADR-0219](0219-session-scope-and-optional-workspace-binding.md) (session scope and optional workspace binding), [ADR-0223](0223-capability-is-the-extension-set.md) (capability is the admitted extension set), [ADR-0240](0240-main-agent-mcp-unification-and-lifecycle-activity-architecture.md) (main agent direct MCP unification)

---

## Context and Problem Statement

Following the main-agent direct MCP unification ([ADR-0240](0240-main-agent-mcp-unification-and-lifecycle-activity-architecture.md)), Model Context Protocol tools execute directly within the master agent's ReAct loop. However, the architectural ownership of MCP configurations remained ambiguous when moving beyond traditional code-repository workflows.

Historically, MCP was conceptualized as a workspace-centric asset: servers were declared in repository files (`.muta/mcp.json`, `.muta/config.toml`) or machine-global configs. This assumed every conversation had an active filesystem project root.

With the separation of conversation scope from workspace bindings ([ADR-0219](0219-session-scope-and-optional-workspace-binding.md)) and the introduction of workspace-free roles such as `philosophist` (`workspace = "none"`), this workspace-centric assumption collapsed:
1. **The Workspace-Free Agent Paradox**: A philosophical agent needs external cognitive capabilities (e.g., PhilPapers, Stanford Encyclopedia of Philosophy, Obsidian notes, arXiv), but possesses no repository or project root. If MCP belongs strictly to a workspace, workspace-free agents are arbitrarily denied external tools.
2. **The Cartesian Configuration Anti-Pattern**: An intuitive temptation arose to introduce "Workspace × Role" configuration matrices (e.g., declaring specific MCP servers for `developer` vs `philosophist` inside `.muta/`), or even "Session-level" persistent MCP declarations. Such approaches generate combinatorial configuration explosion, violate config-versus-state discipline ([ADR-0115](0115-credential-placement-config-vs-state.md)), and create severe user experience fragmentation.
3. **Process Churn vs Cold-Start Latency**: MCP servers are external OS child processes or remote HTTP transports with 500ms–2000ms handshake latencies. If role transitions actively spawn and kill physical processes, terminal interactivity freezes, breaking the zero-latency switching contract.

We require an uncompromised, orthogonal architecture that cleanly decouples where servers live, which roles may access them, and how sessions dynamically mask them.

---

## Decision Drivers

- **Orthogonal Cleanliness**: Eliminate combinatorial configuration matrices (no Workspace × Role schemas, no persistent session configurations).
- **Workspace-Free Parity**: Workspace-free agents (such as `philosophist`) must natively consume global MCP tools without fake working directories.
- **Zero-Latency Role Transitions**: Role switching must never trigger physical process recreation or network reconnection; role-level tool selection must be an $O(1)$ in-memory projection.
- **Strict Separation of Config and State**: Configuration is declared in static, user-editable files (`roles.toml`, `config.toml`, `.muta/mcp.json`). Sessions only hold transient, runtime disable masks (`disabled_tools`).
- **Fail-Closed Security Posture**: Maintain the two-tier trust boundary established in ADR-0240 (trusted user configs vs quarantined workspace configs) and enforce role-level admission filtering before tools enter the model context.

---

## Considered Options

- **Option 1: Workspace × Role Matrix Configuration**  
  Allow `.muta/config.toml` to specify per-role MCP servers (e.g. `[roles.developer.mcp]`).  
  *Rejected*: Causes configuration explosion. Users and tools cannot easily answer which MCP is active in which context, and workspace-free sessions remain unsupported.

- **Option 2: Session-Level Persistent MCP Injection**  
  Allow individual sessions to define and spawn ad-hoc MCP servers stored in the database.  
  *Rejected*: Blurs the line between declarative configuration and immutable event ledgers ([ADR-0115](0115-credential-placement-config-vs-state.md), [ADR-0241](0241-session-ir-causal-graph-and-compiler-pipeline.md)). Resuming sessions across machines or environments fails when local binaries are absent.

- **Option 3: Three-Tier Orthogonal Model (Chosen)**  
  Decouple the problem into three strictly independent layers:
  1. **Environment Server Registry (Where servers live)**: Host daemon manages physical connections from User and Workspace sources.
  2. **Role Capability Subscription (What tools a role needs)**: Roles declare declarative pattern subscriptions (`admit_mcp = ["obsidian", "phil*"]`).
  3. **Session Runtime Masking (What a user temporarily disables)**: Sessions only hold transient disable masks (`disabled_tools`) via interactive toggles (`/mcp`).

---

## Decision Outcome

Chosen option: **Option 3**.

### 1. Three-Tier Architectural Topology

```text
┌─────────────────────────────────────────────────────────────────────────────┐
│ 1. Environment Server Registry (Physical Provisioning Plane)                 │
│    • User-Global Registry (~/.config/muta/config.toml): Implicitly Trusted. │
│      - Personal knowledge, web endpoints: obsidian, philpapers, github.     │
│    • Workspace-Local Registry (.muta/mcp.json) [Optional]: Trust-Gated.     │
│      - Project-specific services: postgres-dev, internal-codegen.           │
│    * Owned and kept alive by the Daemon Host; servers persist across turns. │
└──────────────────────────────────────┬──────────────────────────────────────┘
                                       │ Raw Tool Pool (SourcedTool: Mcp)
┌──────────────────────────────────────▼──────────────────────────────────────┐
│ 2. Role Capability Subscription (Logical Capability Plane)                   │
│    • Declared in roles.toml or built-in MainAgentRole profiles.             │
│    • Roles specify admission patterns:                                      │
│      [roles.philosophist] admit_mcp = ["obsidian", "philpapers", "arxiv"]   │
│      [roles.developer]    admit_mcp = ["*"]                                 │
│    * Evaluated at turn start via O(1) in-memory filter projection.          │
└──────────────────────────────────────┬──────────────────────────────────────┘
                                       │ Admitted Toolset
┌──────────────────────────────────────▼──────────────────────────────────────┐
│ 3. Session Runtime Masking (Execution Interaction Plane)                    │
│    • Interactive modal (/mcp) toggles individual tools via Space bar.       │
│    • Stored in session state as disabled_tools set; zero persistent config. │
│    * Final toolset passed to Model ReAct loop: Admitted ∖ Disabled.         │
└─────────────────────────────────────────────────────────────────────────────┘
```

### 2. Registry vs. Subscription Specifications

#### Environment Server Registry (`config.toml` / `.muta/mcp.json`)
Registers how to connect to or launch servers. It has no awareness of roles or sessions:
```toml
[mcp.obsidian]
command = ["npx", "-y", "obsidian-mcp-server"]

[mcp.philpapers]
url = "https://mcp.philpapers.org/sse"
```

#### Role Capability Subscription (`roles.toml`)
Declares which server patterns are admitted into the role's toolset:
```toml
[roles.philosophist]
name = "Philosophist"
preset = "philosophist"
workspace = "none"
admit_mcp = ["obsidian", "philpapers", "arxiv"]

[roles.developer]
name = "Developer"
preset = "developer"
workspace = "inherit"
admit_mcp = ["*"]
```

### 3. Lifecycle and Zero-Latency Projection Invariant

- **Physical Connection Pool**: The daemon runtime (`McpRuntime`) connects to all configured and trusted MCP servers concurrently in the background and keeps them alive. Idle servers may suspend their background poll loops, but process handles and network sockets remain pooled.
- **In-Memory Tool Projection**: When the agent constructs `visible_tools()` on each turn, MCP tools (`mcp__<server>__<tool>`) are filtered against the active role's `admit_mcp` rules before being checked against `disabled_tools`. Role transitions (e.g. `/role philosophist`) incur $0\text{ ms}$ cold-start cost because no child processes are spawned or killed.

---

## Invariants & Behavioral Boundaries

- **`[INV-MCP-03]` No Cartesian Configuration**: Configuration schemas must never introduce `workspace.roles.*.mcp` or `session.*.mcp`. Server declarations belong exclusively to user/workspace registries, and capability admission belongs exclusively to roles.
- **`[INV-MCP-04]` Registry-Subscription Decoupling**: A server registry declaration never grants unconditional access to every role. Only servers matching a role's `admit_mcp` subscription enter that role's toolset.
- **`[INV-MCP-05]` Workspace-Free Parity**: A session without a workspace (`workspace = None`) has full, unrestricted access to all user-global MCP servers matching its active role. The absence of a workspace must never disable MCP functionality.
- **`[INV-MCP-06]` Zero-Latency Projection**: Switching active roles (`apply_role`) must never restart, terminate, or reconnect underlying MCP transport processes. Tool visibility is strictly an in-memory set projection.
- **`[INV-MCP-07]` Session Mask Ephemerality**: Sessions may only record name-level disable overrides (`disabled_tools`). No session may serialize or persist executable process paths or server environment configurations.

---

## Positive Consequences

- Workspace-free roles like `philosophist` cleanly and natively possess specialized external tools without requiring synthetic or empty directory bindings.
- Role switching retains immediate, sub-millisecond responsiveness without terminal activity bar stutter or transport handshakes.
- Eliminates configuration duplication: a team sharing a repository `.muta/mcp.json` automatically scopes its tools to roles without project-specific role overrides.
- Total architectural alignment with the homogeneous agent kernel ([ADR-0183](0183-homogeneous-agent-kernel-and-spatiotemporal-aspect-engine.md)) and clean-break documentation governance.
