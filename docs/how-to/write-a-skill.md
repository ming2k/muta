# How to write a skill

This guide walks through authoring a skill: where to put it, how the harness
finds it, and the minimal frontmatter it needs. A skill is a Markdown document
with a small YAML header; when the agent needs that expertise, the document body
is injected into the conversation. Skills carry no executable code. For the
conceptual model, see [Skills](../explanation/agent-design/skills.md); for the
lookup-oriented path map, see [Paths](../reference/paths.md).

## Put the skill where the harness looks

A skill is a `SKILL.md` file inside its own directory (the directory may hold
auxiliary files the body references). Pick a location by how broadly the skill
should apply:

| Scope | Location | Use when |
|-------|----------|----------|
| **Project-local** | `<project>/.muta/skills/<name>/SKILL.md` | The skill belongs to one project and should travel with the repo |
| **User-global** | `$XDG_DATA_HOME/muta/skills/<name>/SKILL.md` | The skill should be available to every project under your user |

`$XDG_DATA_HOME` resolves to `~/.local/share` on Linux by default, so the
user-global location is normally `~/.local/share/muta/skills/<name>/SKILL.md`.
Both paths resolve through the central `Dirs` layer and honour the standard XDG
overrides; see [Paths](../reference/paths.md) for the full stack.

Project-local skills override user-global skills with the same name — the most
specific source wins. Pick user-global only when you genuinely want the skill
everywhere.

## Where you do not need to put anything

Three other locations appear in the discovery cascade, but none of them is an
author target:

- `~/.cache/...` (more precisely `$XDG_CACHE_HOME/muta/skills/remote/`) is the
  **cache** for remote skill repositories fetched from `[skills] urls`. It is
  derived, deletable, and repopulated on demand. Never hand-author a skill
  there; a refetch overwrites it.
- `~/.agents/skills/` and `~/.claude/skills/` (and the project-local
  `.agents/skills/`, `.claude/skills/` counterparts) are **other tools'**
  conventions. muta reads them so a shared skill library works across agents,
  but it never writes to them. You do not need them; the project-local
  `.muta/skills/` and user-global XDG locations cover every authoring case.

If you want skills sourced from somewhere else entirely (a private repo, a
shared team directory), configure it as an extra scan path under
`[skills] paths` in `config.toml` rather than copying into a cache or a foreign
directory.

## Write the file

A skill with frontmatter looks like this:

```text
---
name: my-skill
description: Use when the task involves <specific situation>
short-description: Short help
version: "1.0.0"
tags: [rust, testing]
policy:
  allow_implicit_invocation: true
dependencies:
  - type: mcp
    value: context7
---
The Markdown body goes here. This is the expertise injected into the
conversation when the skill is invoked. Reference auxiliary files in the same
directory by relative path.
```

The meaningful frontmatter fields:

| Field | Purpose |
|-------|---------|
| `name` | Identity used for invocation and override. If omitted, the parent directory name is used. |
| `description` | One line shown to the model when it decides relevance. The most important field for triggering accuracy. |
| `short-description` | Fallback for the catalog when `description` is empty. |
| `policy.allow_implicit_invocation` | Whether the skill may auto-load when its name is mentioned (default `true`). |
| `dependencies` | Tools the skill expects to be available (e.g. an MCP server). Declarative; not yet enforced. |
| `tags`, `version` | Metadata. |

A skill with no frontmatter is still valid: the file becomes the whole body and
the name is derived from the parent directory.

## How discovery finds your skill

The skill registry (`crates/muta-skills/src/discovery.rs`) scans each
source directory **recursively**. Any file named exactly `SKILL.md` is parsed
as a skill, regardless of how deeply it is nested. This means a single scan
root can hold many skills side by side, and skills can live in nested
subdirectories:

```text
~/.local/share/muta/skills/
├── rust-expert/
│   └── SKILL.md
├── team/
│   ├── onboarding/
│   │   └── SKILL.md
│   └── ci/
│       └── SKILL.md
└── another/yy/zz/
    └── SKILL.md
```

All of the `SKILL.md` files above are discovered. For each one, the **parent
directory** of the file becomes the skill's root (where relative references
resolve), and the skill's name comes from frontmatter `name` or — when absent —
from that parent directory name.

Two rules worth knowing:

- **Hidden directories are skipped.** Any path component that starts with a dot
  (`.`) causes the whole subtree under it to be ignored, so `.git`, `.archive`,
  or a `drafts/old/` folder renamed to `.old` will not surface skills.
- **Body is loaded lazily.** Discovery reads only the frontmatter. The Markdown
  body enters memory the first time the skill is actually invoked, so a large
  catalog costs nothing until it is used.

When two sources declare a skill with the same `name`, the higher-priority
source overrides the lower one in place (the catalog position of the first
claim is kept). The full cascade, lowest priority first, is documented in
[Sources and priority](../explanation/agent-design/skills.md#sources-and-priority).

## Invoke and reload

Two paths take a discovered skill into context:

- **Explicit** — the model calls the `use_skill` tool with the skill name.
- **Implicit** — the harness scans the latest user message for skill mentions
  (`@skill-name`, the disambiguated `@skill:name` / `@skills:name`, or a
  `skill://` URI) and auto-loads any mentioned skill whose
  `policy.allow_implicit_invocation` is true. A plain name occurrence does not
  trigger loading.

Newly added, removed, or edited skill files are picked up automatically without a restart
via the daemon's reactive file watching. For project-local skills in new workspaces, run `/trust skills` to authorize them.

## See also

- [Skills](../explanation/agent-design/skills.md) — the two-channel model and
  source priority cascade.
- [Paths](../reference/paths.md) — where skills live across the XDG layout.
- [Skills tools](../reference/tools/skills.md) — the `use_skill` and
  `list_skills` tool contracts.
