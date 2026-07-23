# neenee-skills

Skill metadata, discovery, remote caching, live registries, periodic refresh,
and the `use_skill` / `list_skills` tool adapters.

The crate depends on core contracts and store-owned XDG paths, never on agent
or session orchestration. `neenee-agent` consumes a `SkillRegistry` for
model-context injection through `AgentBuilder::with_skills`.
