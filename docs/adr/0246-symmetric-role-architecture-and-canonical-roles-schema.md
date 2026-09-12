# 0246. Symmetric Role Architecture, Pure-Allowlist Capability Projection, and Canonical roles.toml Schema

- **Status:** Accepted
- **Date:** 2026-10-03
- **Scope:** `core/contracts`, `core/agent`, `storage/persistence`, `runtime/assembly`, `runtime/slash`, `terminal/mutx`
- **Deciders:** Muta Architecture Team
- **Builds on:** [ADR-0220](0220-personas-persisted-named-principals.md) (personas and roles), [ADR-0242](0242-orthogonal-mcp-registry-and-role-capability-subscription.md) (orthogonal role capability), [ADR-0244](0244-immutable-session-role-and-truthful-identity-projection.md) (immutable session role and truthful projection), [ADR-0245](0245-hermetic-session-role-manifest-snapshotting.md) (hermetic session role manifest snapshotting)

---

## Context and Problem Statement

Historically, Muta’s concept of agent specialization evolved through a series of tactical patches: from early "personas" (`personas.toml`) to "agent presets" and eventually to "roles" (`roles.toml`). While [ADR-0244](0244-immutable-session-role-and-truthful-identity-projection.md) and [ADR-0245](0245-hermetic-session-role-manifest-snapshotting.md) established session immutability and hermetic snapshotting, an architectural audit revealed that the underlying role entity model remained encumbered with cognitive debt, conceptual leakage, and legacy asymmetries:

1. **The "Preset" Hierarchical Anti-Pattern**: The runtime treated `developer` and `philosophist` as privileged, hardcoded "presets", forcing every user-authored role in `roles.toml` to declare `preset = "developer"` or `preset = "philosophist"`. This created a confusing, leaky abstraction where custom roles were second-class citizens subclassing arbitrary compiler constants, rather than self-sovereign entities.
2. **Conflated Operational and Identity Concerns**:
   - **`unattended` in Static Config**: `unattended` (autonomous execution) is a transient operational mode of a live session (`--unattended`, `/unattended`), representing whether a human is attending the channel. Hardcoding `unattended = true|false` into a static role configuration conflated identity with execution posture.
   - **Model and Provider Coupling**: Roles historically declared `connection` and `model` overrides. In practice, model and inference parameters (temperature, thinking budget) are infrastructure concerns managed globally (`config.toml`) or dynamically via the model picker (`/models`). Binding physical models to roles distorted the orthogonal boundary between "who the agent is" and "what engine powers it".
3. **Ambiguous Instruction Hierarchy (`mission` vs `directive` vs `persona`)**: Roles carried fragmented prompt fields (`mission`, `directive`, legacy `persona` aliases) with murky override precedence. The model request assembler had to evaluate conditional fallbacks to construct the Base-Tier preamble.
4. **Asymmetric Tool Filtering (MCP Only vs. Native Tools Locked)**: While [ADR-0242](0242-orthogonal-mcp-registry-and-role-capability-subscription.md) introduced `admit_mcp` for external MCP tools, native tools (filesystem operations, bash execution) were rigidly dictated by the `preset`. Users could not declare a read-only code review role or an exploratory search role without native tool leakage. Furthermore, security capability models must be **pure allowlists**: denylists/blacklists provide a false sense of security, introduce cognitive friction, and violate least-privilege principles.
5. **Inconsistent Identifier Syntax**: Role IDs allowed arbitrary snake_case or mixed punctuation, drifting from Muta's canonical kebab-case CLI and resource identifier standard.

We require a clean-break architecture governed by long-termism, zero legacy compromises, and absolute conceptual purity.

---

## Decision Drivers

- **First-Class Entity Symmetry**: Every role in Muta—whether built-in (`developer`, `philosophist`) or user-defined (`code-reviewer`, `security-auditor`)—is an equal, first-class entity instantiated from one canonical schema. There are no "presets" or privileged subclasses.
- **Pure-Allowlist Capability Model**: Capability admission is strictly positive (allowlist-only). A role explicitly enumerates the tools and MCP servers it admits. Blacklists/denylists are rejected.
- **Orthogonal Separation of Concerns**: Static role definitions define *identity* (`instructions`), *context binding* (`workspace`), and *capabilities* (`tools`, `admit_mcp`). Execution modes (`unattended`), model routing (`connection`), and runtime options belong to their respective layers.
- **Kebab-Case Identifier Invariant**: Role IDs are strictly kebab-case (`^[a-z0-9]+(-[a-z0-9]+)*$`).
- **Deterministic Prefix-Cache Stability**: Base-tier system instructions are explicit and stable, guaranteeing maximal KV cache reuse across turns.

---

## Decision Outcome

We execute a complete clean-break migration across contracts, persistence, agent, and runtime.

### 1. The Canonical `roles.toml` Schema

The configuration file `$XDG_CONFIG_HOME/muta/roles.toml` (and workspace overrides `.muta/config.toml`) adheres to the following clean schema:

```toml
# ~/.config/muta/roles.toml

[roles.code-reviewer]
name = "Code Reviewer"
description = "Specialized role for code quality, security audits, and architectural reviews"
workspace = "inherit"

instructions = """
Role: code-reviewer. Inspect git diffs and repository files.
Identify security hazards, performance regressions, and architectural anti-patterns.
Guide developers with constructive, surgical feedback.
"""

tools = [
    "read_text",
    "find_files",
    "search_text",
    "code_query",
    "ask_user"
]

admit_mcp = ["github", "gitlab"]
```

### 2. Elimination of `preset`, `unattended`, and `model` in Role Definitions

- **No `preset`**: The `preset` field is deleted. All roles directly define their `workspace`, `instructions`, and `tools`.
- **Built-in Roles as Standard Profiles**:
  - `developer`: `workspace = "inherit"`, `tools = ["*"]`, `admit_mcp = ["*"]`.
  - `philosophist`: `workspace = "none"`, `tools = ["ask_user"]`, `admit_mcp = []`.
- **No `unattended`**: Autonomous mode is governed exclusively by CLI flag `--unattended` or slash command `/unattended`.
- **No `model` / `connection`**: Model selection remains entirely orthogonal and governed by `/models` and `config.toml`.

### 3. Pure-Allowlist Capability Projection

- `tools: Vec<String>`: A list of exact tool names (`"read_text"`, `"ask_user"`) or glob patterns (`"read_*"`, `"search_*"`, `"*"`).
  - An absent or `["*"]` declaration grants unrestricted access to registered native tools.
  - An explicit list (e.g. `["read_text", "code_query", "ask_user"]`) admits *only* the matched tools. Unmatched tools are physically omitted from the model request's tool schema array.
- `admit_mcp: Vec<String>`: Allowlist patterns for MCP servers (e.g. `["github", "obsidian*"]`, or `["*"]`).

### 4. Canonical Field Taxonomy

| Field | Type | Default | Semantics |
| :--- | :--- | :--- | :--- |
| `name` | `String` | `id` | Human-readable display label in UI chrome |
| `description` | `Option<String>` | `None` | Short summary displayed in `/role` listing |
| `instructions` | `Option<String>` | `None` | System instructions injected at `InstructionTier::Base` (`Head`) |
| `workspace` | `RoleWorkspace` | `Inherit` | `"inherit"` (workspace-bound), `"none"` (workspace-free), or explicit path |
| `tools` | `Vec<String>` | `["*"]` | Allowlist of admitted native tools |
| `admit_mcp` | `Vec<String>` | `["*"]` | Allowlist of admitted MCP server names |

### 5. Strict Kebab-Case Validation

All role identifiers are validated by the strict regex `^[a-z0-9]+(-[a-z0-9]+)*$`. IDs with underscores, uppercase characters, or invalid punctuation are rejected at parse time with actionable errors.

---

## Consequences

### Positive
- **Architectural Purity**: Elimination of the "preset" hierarchy establishes true symmetry across all built-in and user-defined roles.
- **Zero-Trust Least Privilege**: The pure allowlist model guarantees that specialized roles (reviewers, tutors, triagers) cannot execute destructive mutations, even if prompted by an adversarial injection.
- **Elimination of Cognitive Debt**: Developers and users have exactly one intuitive model for roles: Name, Description, Instructions, Workspace, Tools, and MCP.
- **Cache Optimization**: Unambiguous `instructions` mapping directly to `InstructionTier::Base` ensures optimal KV prefix-cache hits.

### Negative / Clean-Break Impact
- Legacy `preset`, `directive`, `mission`, `persona`, and `unattended` keys in existing `roles.toml` files are dropped without backward-compatibility shims, requiring users to format according to the canonical schema.
