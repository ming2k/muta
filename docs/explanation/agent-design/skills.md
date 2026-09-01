# Skills

Skills are on-demand domain expertise. A skill is a Markdown document with a
small YAML header; when the agent needs that expertise, the document body is
injected into the conversation so the model can act on it. Skills are *not*
tools — they carry no executable code. They steer the model by adding
instructions, and the model then uses ordinary tools (execute_command, edit, ...) to do
the work those instructions describe.

This page covers where skills come from, how they are ordered, and how a skill
body reaches the model. For the lookup-oriented file layout, see
[Paths](../../reference/paths.md); for the `use_skill` tool contract, see
[Skills and `use_skill`](../../reference/tools/skills.md).

## On-demand discovery and loading

Skill metadata and skill bodies enter model context only when needed:

| Path | What it carries | Where it lands | When |
|------|-----------------|----------------|------|
| **Discovery** | Skill names, scopes, descriptions, and enabled state | A `list_skills` tool result | When the model asks what is available |
| **Body** | The full Markdown expertise document | A `use_skill` tool result or hidden user message | When explicitly loaded or mentioned |

The system prompt carries neither metadata nor skill bodies. Discovery is
delegated to the tool surface, avoiding a repeated catalog cost on every
provider request. Bodies remain lazy and are cached after their first load.

## Sources and priority

Skills are discovered from several sources. Each source is labelled with a
**scope**, and scopes are ordered: a higher-priority scope overrides a
lower-priority scope when two skills share a name.

| Scope | Source | Priority |
|-------|--------|----------|
| **Remote** | Skill repositories fetched from `[skills] urls` and cached locally | Lowest |
| **User** | User-global skills: the XDG data dir, plus external application conventions (`~/.agents/skills/`, `~/.claude/skills/`) | |
| **Extra** | Extra paths configured under `[skills] paths` | |
| **Repo** | Project-local skills in the project working tree (`.muta/skills/`, `.agents/skills/`, `.claude/skills/`) | Highest |

The intent of the cascade is that the most specific source wins: a skill
checked into a project overrides a user-global skill with the same name, which
in turn overrides a remote one.

Two design notes worth calling out:

- **External directories are read-only.** `~/.agents/skills/` and
  `~/.claude/skills/` (and their project-local `.agents/skills/`,
  `.claude/skills/` counterparts) are other tools' conventions. muta reads
  them so a shared skill library works across agents, but it never writes to
  them.

All user-level paths resolve through the central `Dirs` layer and honour the
standard XDG overrides. See [Persistence and the XDG
layout](../persistence.md) for the override stack.

## The skill format

A skill is a `SKILL.md` file inside its own directory (so it can carry
auxiliary files the body references). The YAML frontmatter declares who the
skill is and how it behaves; the Markdown body is the expertise itself.

The meaningful frontmatter fields:

| Field | Purpose |
|-------|---------|
| `name` | Identity used for invocation and override. If omitted, the parent directory name is used. |
| `description` | One line shown in the catalog and used to decide relevance. |
| `short-description` | Fallback for the catalog when `description` is empty. |
| `policy.allow_implicit_invocation` | Whether the skill may auto-load when its name is mentioned (default true). |
| `dependencies` | Tools the skill expects to be available (e.g. an MCP server). Declarative; not yet enforced. |
| `tags`, `version` | Metadata. |

A skill with no frontmatter is still valid: its body becomes the whole file and
its name is derived from the directory.

## How a skill is invoked

There are two paths from identifying a skill to placing its body in context:

1. **Explicit — `use_skill`.** The model calls the `use_skill` tool with a
   skill name. The tool looks up the skill, returns its body as a tool result,
   and also lists the auxiliary files in the skill directory. `use_skill` is an
   ordinary read-only tool, architecturally identical to `read_text` or `execute_command`;
   its only specialty is that its result happens to be a skill body. This works
   even for disabled skills, so the model can load one and explain why it did
   nothing.

2. **Implicit — mention detection and completion.** Before a round runs, the harness scans the
   latest visible user message for skill mentions. A mention is one of:
   - an `@skill-name` reference,
   - the disambiguated `@skill:skill-name` / `@skills:skill-name` namespace,
   - a `skill://skill-name` or source-path URI.

   **Composer Autocompletion**: The composer provides cursor-anchored, inline overlay autocompletion for `@skill:` / `@skills:` and `@` mentions anywhere within multiline input. Selecting a candidate commits the unambiguous `@skill:<name>` token.

   Each mentioned skill whose policy allows implicit invocation is loaded as a
   **hidden user message** carrying the same `[Skill '<name>' loaded]` marker
   the explicit path uses. Hidden means it steers the model but is not rendered
   as part of the visible transcript. A plain name occurrence is deliberately
   ignored because common words would otherwise pull large bodies into context
   accidentally. Already implicitly loaded skills are not re-injected.

3. **Dynamic Discovery & Runner Delegation.** The system prompt stays clean and free of static `<available_skills>` catalog bloat. Instead, when domain expertise is required, the principal agent can spawn a dedicated runner with `role: "skill"` (`RUNNER_SKILL`) to dynamically discover, inspect, and extract instructions from project (`.muta/skills/`) and user (`$XDG_DATA_HOME/muta/skills/`) skill trees, returning synthesized guidelines directly into the execution flow. Global and local skill roots are natively admitted to the sandbox.

Both paths emit the same marker, so persisted context remains auditable even
though one path is a tool result and the other is harness-authored user context.

## Policy and enabled state

Two flags govern visibility:

- **`enabled`** (default true). A disabled skill remains visible through
  `list_skills` with its disabled state and is never auto-loaded on mention. It
  can still be requested explicitly via `use_skill`. Skills can be disabled
  through configuration (`[skills] disable`).
- **`allow_implicit_invocation`** (default true). When false, the skill remains
  discoverable and responds to `use_skill`, but mention detection skips it.
  Use this for skills that should only be loaded deliberately.

A skill participates in implicit invocation only when it is both enabled and
allows it.

## Workspace Trust and Quarantine Transparency

Project-local skills (`.muta/skills/`, `skills/`) belong to the workspace's project asset domain (ADR-0145).
Discovery scans project directories regardless of trust state:
- When the workspace skills domain is **Trusted**, project skills are enabled and available for invocation.
- When the workspace skills domain is **Quarantined** (e.g. fresh clone or modified project files), project skills are recorded as `Quarantined` and remain disabled from model invocation.
- Quarantined skills are displayed in the TUI `/skills` modal and `muta skill ls` with an explicit `Quarantined` badge and remediation guidance (`Run /trust skills to enable`).

## Reactive State and Trust Lifecycle

Skill discovery is event-driven and reactive:
- **Filesystem Changes**: The daemon watches skill roots with platform-native, kernel-level file watchers (Linux inotify, macOS FSEvents, Windows IOCP/ReadDirectoryChangesW) and updates the in-memory registry automatically.
- **Trust Authorization**: Authorizing project assets via `/trust skills` or `/trust` triggers security re-attestation and updates skill admission state.
- **No Manual Reload**: Manual reload commands (`/skills reload`) are eliminated in favor of continuous, reactive synchronization.

## Decision history

- [ADR-0165](../../adr/0165-reactive-skills-architecture-and-decoupled-security.md) — reactive skills architecture, decoupled security, and transparent quarantine.
- [ADR-0058](../../adr/0058-remove-bundled-skill-tier.md) — retain XDG skill
  paths while removing the unused bundled-system tier.
- [ADR-0014](../../adr/0014-xdg-persistence-architecture.md) — the unified XDG
  persistence architecture that all skill paths resolve through.

## Adjacent layers

Skills are an **extension surface** of the harness, alongside MCP servers (which
add tools, not instructions). Skill discovery and explicit loading use the tool
surface; implicit loading enters through model-context preparation. See
[Prompt and message assembly](prompt-assembly.md). Skill invocation is a
special case of a tool turn, so [Rounds and turns](rounds-and-turns.md) describes the
execution path an explicit `use_skill` call takes.
