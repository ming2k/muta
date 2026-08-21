# Skills tools

Skills are not tools, but two small tools manage them, and a third searches
session history. All are `Read` and bypass the permission broker.

## `use_skill`

| Parameter | Type | Required | Notes |
|-----------|------|----------|-------|
| `name` | string | yes | Skill name from frontmatter |

`UseSkillTool` (`crates/neenee-skills/src/tools.rs`) loads the skill body
into the conversation.

## `list_skills`

| Parameter | Type | Required | Notes |
|-----------|------|----------|-------|
| — | — | — | No parameters |

`ListSkillsTool` lists every available skill with its scope, description, and
enabled state. Useful for seeing what the agent can load before calling
`use_skill`.

## Skill format

A skill is a Markdown file with YAML frontmatter, conventionally named
`SKILL.md` inside a skill directory:

```text
.neenee/skills/<name>/SKILL.md                          # project-local
$XDG_DATA_HOME/neenee/skills/<name>/SKILL.md            # user-global
```

```text
---
name: my-skill
description: When to invoke this skill
short-description: Short help
version: "1.0.0"
tags: [rust]
policy:
  allow_implicit_invocation: true
dependencies:
  - type: mcp
    value: context7
---
Skill body injected into the context on demand.
```

## Discovery

The skill registry (`crates/neenee-skills/src/discovery.rs`) discovers
skills from, in priority order (later sources override earlier ones):

1. **Remote skill repositories** configured under `[skills] urls` in
   `config.toml`, cached under `$XDG_CACHE_HOME/neenee/skills/remote/`.
2. **External user-global formats** — `~/.agents/skills/`, `~/.claude/skills/`
   (someone else's app convention).
3. **User-global skills (XDG)** — `$XDG_DATA_HOME/neenee/skills/`
   (`~/.local/share/neenee/skills/` on Linux by default).
4. **Extra local paths** configured under `[skills] paths` in `config.toml`.
5. **External project formats** — `.agents/skills/`, `.claude/skills/` in the
   project root.
6. **Project-local neenee skills** — `.neenee/skills/` in the project root
   (highest priority).

All user-level paths resolve through the central `Dirs` layer
(`crates/neenee-persistence/src/paths.rs`) and honour the standard XDG overrides
(`$XDG_DATA_HOME`, `$XDG_CACHE_HOME`) plus the app-specific overrides
(`$NEENEE_DATA_DIR`, `$NEENEE_CACHE_DIR`). See [Paths](../paths.md) for
the full override stack and [Persistence and the XDG
layout](../../explanation/persistence.md) for the conceptual model.

The catalog is **not** injected into the system prompt. Skills are discovered
on demand via `list_skills` and loaded via `use_skill`. Skills whose names are
explicitly mentioned with `@skill-name`, the disambiguated `@skill:name` /
`@skills:name`, or a `skill://` URI are auto-loaded during model-context
preparation when their policy allows implicit invocation.
