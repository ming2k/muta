# 0058. Remove the bundled skill tier

- **Status:** Accepted
- **Date:** 2026-07-13

## Context

ADR-0013 established XDG paths for user-authored skills and a compile-time
bundled-system tier. No bundled `SKILL.md` has shipped: the embedded directory
contains only a placeholder, `skills.bundled` defaults to false, and enabling
it discovers zero skills.

Keeping an empty source adds a configuration key, a `SkillScope::System`
variant, an embedding dependency, discovery branches, and documentation for a
capability users cannot exercise. It also implies that neenee owns a built-in
expertise catalog when all current skills come from external repositories or
the user's filesystem.

## Decision

Retain the XDG and project-path decisions from ADR-0013, but remove its bundled
skill decision.

Skill discovery has four scopes, from lowest to highest priority: `Remote`,
`User`, `Extra`, and `Repo`. Remove `SkillScope::System`, the `skills.bundled`
configuration field, the compile-time loader and placeholder directory, and
the `include_dir` dependency.

Existing configuration files that still contain `bundled` remain readable;
Serde ignores the unknown field under the current configuration schema.

## Alternatives considered

### Keep the empty tier for future skills

Rejected because a future bundled catalog should justify its product and
update behavior when it exists. Carrying a no-op public surface does not make
that future implementation cheaper.

### Ship a minimal bundled skill now

Rejected because no built-in expertise has a defined owner, update policy, or
compatibility contract. Adding content only to justify the mechanism reverses
the dependency between need and design.

## Consequences

**Positive.** The runtime and documentation describe only skill sources that
can produce skills today.

**Positive.** The agent drops one dependency and one public configuration
field.

**Negative.** Code matching exhaustively on `SkillScope` must remove the
`System` arm. Configuration generators must stop emitting `bundled`.

**Neutral.** User-global, external-format, remote, extra-path, and
project-local discovery behavior is unchanged.

## References

- [ADR-0013](0013-skills-xdg-paths-and-bundled-embed.md)
- [Skills](../explanation/agent-design/skills.md)
- [Paths](../reference/paths.md)
