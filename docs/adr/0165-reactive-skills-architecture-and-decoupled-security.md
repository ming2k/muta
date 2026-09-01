# 0165. Reactive Skills Architecture, Decoupled Security, and Transparent Quarantine

- **Status:** Accepted
- **Date:** 2026-07-20

## Context

Muta's skill system has evolved through several milestones (ADR-0013, ADR-0058, ADR-0107, ADR-0145). As skills become the primary medium for domain expertise and standard operating procedures (SOPs), several legacy assumptions and coupling points created friction:

1. **Tool Coupling in System Prompt**: `SkillsGuidance` required `read_text` or `read_file` to be present in `ctx.tool_names` before injecting skills metadata. This conflated domain knowledge with execution tools, penalizing restricted agents (e.g. specialized review agents without general file-reading tools) and ignoring implicit mention injections.
2. **Silent Quarantine (Invisible Skills)**: When a workspace's skills domain was untrusted, project-level skills (`.muta/skills/`) were completely skipped during discovery. Users authoring or discovering project skills experienced cognitive dissonance—believing the parser or path resolution was broken, rather than understanding that security attestation was required.
3. **Manual Reload Anti-Pattern**: An interactive `/skills reload` slash command and corresponding `r` keybinding in the TUI created a clunky manual lifecycle. In a reactive daemon architecture where filesystem events are observed and trust transitions are discrete, manual reload is redundant and masks security boundaries.
4. **Command Grammar Ambiguity**: The distinction between plural aggregate views (`/skills` for directory/modal browsing) and singular entity actions (`/skill show`, `muta skill init`) was blurred.

## Decision

1. **Decouple Tool Checks from Skills Guidance**: Remove the requirement for `read_text` or `read_file` in `SkillsGuidance::is_active`. Skills metadata availability is an infrastructure-level domain property, independent of session tool allocations.
2. **Transparent Quarantine Discovery**: Discover all project-local skills regardless of workspace trust state. If the workspace trust state for the skills domain is `Quarantined`, the skills are recorded with `enabled: false` and explicitly surfaced in TUI modals and CLI listings with a `Quarantined` status badge and clear actionable remediation (`Run /trust skills to enable`).
3. **Eliminate Manual Reload**: Remove the `/skills reload` command and TUI `r` keybinding. Lifecycle updates are driven purely by reactive workspace events and authoritative `/trust` attestation state transitions.
4. **Command Grammar and Discovery Simplification**:
   - `/skills`: Read-only aggregate inspection (opens the centered TUI modal or prints summary tables).
   - `@skill:<name>` (and `@name`): First-class mention syntax for binding skills directly into user prompts.
   - `RUNNER_SKILL` (`role: "skill"`): Dedicated sub-runner role for dynamic discovery, extraction, and synthesis of skill procedures on demand, removing static catalog clutter from the system prompt.
   - `muta skill init <name>` & `muta skill check <path>`: First-party CLI tooling for authoring and validating skills. Singular slash command `/skill` is completely retired.

## Alternatives considered

### Keep `/skills reload` as a fallback
Rejected. Retaining manual reload encourages users to treat reload as a debugging crutch instead of understanding security trust boundaries and reactive state sync.

### Hide quarantined skills until `/trust`
Rejected. Silent omission breaks user feedback loops when creating or editing project-level skills.

## Consequences

**Positive.**
- Skills are visible and discoverable across all agent profiles, including restricted agents.
- Quarantined project skills provide clear, immediate feedback with self-serve `/trust skills` hints.
- Zero manual reload churn; state transitions are reactive and authoritative.
- Clear, unified entry point: `/skills` for browsing, `@skill:<name>` for prompt binding, and `RUNNER_SKILL` for dynamic agent discovery.

**Negative.**
- Users accustomed to typing `/skills reload` or pressing `r` in the modal must rely on reactive updates or `/trust`.

**Neutral.**
- Workspace security attestation boundaries (ADR-0145) remain strictly enforced. Untrusted skills cannot execute or inject bodies.

## References

- [ADR-0013: Skills XDG Paths and Bundled Embed](0013-skills-xdg-paths-and-bundled-embed.md)
- [ADR-0058: Remove the Bundled Skill Tier](0058-remove-bundled-skill-tier.md)
- [ADR-0107: Trust Gate Covers Project Skills and Commands](0107-trust-gate-covers-project-skills-and-commands.md)
- [ADR-0145: Decoupled Workspace Asset Trust and Tool Hazard Model](0145-decoupled-workspace-asset-trust-and-tool-hazard-model.md)
- [Skills Architecture Guide](../explanation/agent-design/skills.md)
