# 0261. Purge of Nanny Prompting and Attention-Frugal Context Architecture

- **Status:** Accepted
- **Date:** 2026-09-19
- **Implementation:** `muta-agent`
- **Builds on:** [ADR-0056](0056-model-context-assembly-boundary.md), [ADR-0160](0160-platform-abstraction-and-shim-layer.md)
- **Amends:** [ADR-0257](0257-unbounded-stream-guard-and-finite-execution-contract.md) (removes redundant system prompt axiom in favor of physical runtime enforcement and self-healing advisory)

## Context

Over successive iterations of agent development, the system prompt accumulated layers of behavioral guidance, operational instructions, and negative constraints:
- `HostEnvironmentGuidance` (`system.host_environment`): Formatted Markdown declarations of the host workspace, OS, and shell, accompanied by explicit `Tool Guidance` ("ALWAYS prefer built-in tools over shell commands") and the `Finite Foreground Execution Axiom` warning against streaming commands.
- `PersistenceGuidance` (`system.persistence`): Exhortations to "see the task through to a real result" and not stop at partial fixes.
- `DelegationGuidance` (`system.delegation_guidance`): Instructions dictating when to delegate searches to subagents versus performing direct tool queries.
- `FileEditingGuidance` (`system.file_editing_guidance`): Instructions dictating tool preference for `edit_text` over `write_file` and forbidding shell redirection.

While originally justified under the engineering adage *"Explicit is better than implicit"*, applying this doctrine indiscriminately to LLM system prompts introduced a fundamental category error: confusing protocol and interface explicitness with prompt micromanagement ("nanny prompting").

In practice, nanny prompting introduces severe negative side-effects:
1. **Attention Cannibalization & Context Bloat**: Large blocks of negative rules and operational lecturing consume model attention heads before the user's task is even read, degrading instruction adherence on the actual task.
2. **Double-Buffering Runtime Guards**: When physical runtime guards (such as ADR-0257 StreamGuard, process timeouts, and tool input validators) already enforce execution safety and emit targeted self-healing diagnostics on failure, reciting those rules in advance in the system prompt is purely redundant noise.
3. **Fragile Preachings over Self-Perception**: Modern reasoning models reliably deduce execution environments through standard tool invocation (e.g. running commands from the workspace root) and tool schema semantics without requiring upfront essay-length prompts.

## Decision

We thoroughly and unconditionally purge all nanny prompting from the default system prompt registry and establish an attention-frugal context architecture.

### 1. New Design Principles

1. **Mechanisms over Exhortations**:
   Safety, sandboxing, and execution bounds must be enforced by physical runtime mechanisms (sandboxes, watchdog timeouts, StreamGuard cadence limits, schema validators), never by pleading with the model in prompt text. Runtime failures must emit immediate, actionable self-healing guidance directly in the tool result rather than upfront in the system prompt.
2. **Attention Frugality & Signal Maximization**:
   Every token in the system prompt must justify its presence as an indispensable structural identity or security boundary. The system prompt is not a repository for operational advice or cheerleading.
3. **Tool Autonomy & Locality**:
   Tool usage guidelines belong in individual tool descriptions and parameter schemas where they are evaluated locally during tool selection, rather than broadcast globally into the system prompt.

### 2. Purged Policy Sections

The following four legacy sections are completely eliminated from `crates/muta-agent`:
- `HostEnvironmentGuidance` (`system.host_environment`)
- `PersistenceGuidance` (`system.persistence`)
- `DelegationGuidance` (`system.delegation_guidance`)
- `FileEditingGuidance` (`system.file_editing_guidance`)

No inert stub structs or dummy `None` renders are retained; the default prompt registry is unburdened by legacy artifacts.

### 3. Permitted System Prompt Surface

The default system prompt registry is restricted to:
- **`system.identity_preamble`** (Base Tier): Inactive by default. Populated only when the embedding or user explicitly configures a principal role or subagent directive.
- **`system.tone`** (Base Tier): Reserved slot for future mission-neutral output tone directives (inactive).
- **`system.model_guidance`** & **`system.provider_guidance`** (Base Tier): Model- or provider-specific wire formatting adjustments when declared by the provider adapter.
- **`system.project_rules`** (Session Tier): Injected strictly from user-authored project rules (e.g. `AGENTS.md`).
- **`system.workspace_roots`** (Session Tier): Admitted cross-project directory paths when multi-workspace access is configured, establishing explicit sandbox security boundaries.
- **`system.web_untrusted_content`** (Session Tier): Untrusted web content boundaries when web access tools are loaded, preventing prompt injection from fetched pages.

When an agent initializes without an explicit identity preamble or project rules, the assembled system prompt is completely empty, ensuring zero initial prompt overhead and 100% context allocation to the user.

## Invariants

- A default agent with no identity directive or project rules emits zero system message tokens.
- No system prompt section may instruct the model to prefer one native tool over another; tool preference is governed by tool descriptions and runtime feedback.
- Physical execution constraints (ADR-0257 StreamGuard, idle watchdogs, and timeout governors) operate independently of system prompt content.
- Negative behavioral constraints are rejected from the core system prompt registry.
