# How-to guides

Task-oriented guides for extending neenee. Each guide assumes familiarity
with the relevant reference material.

| Guide | Task |
|-------|------|
| [How to add a built-in tool](add-a-tool.md) | Implement the `Tool` trait, pick a `ToolAccess`, register, verify |
| [How to add a provider](add-a-provider.md) | Wrap `OpenAiChatCompletionsProvider` or build a standalone adapter, register dispatch sites |
| [How to ask the user a question during a task](ask-the-user.md) | Use `ask_user` to resolve ambiguity or collect preferences mid-task |
| [How to configure TUI appearance](configure-tui-appearance.md) | Apply a color preset or edit a custom semantic palette from `/config` |
| [How to use sub2api relays](use-sub2api.md) | Configure OpenAI, Anthropic, and Google-compatible sub2api relays |
| [How to write a skill](write-a-skill.md) | Author a `SKILL.md` skill: frontmatter, policy, explicit vs implicit invocation, reload |
| [How to track sessions with a session host](track-sessions-with-a-session-host.md) | Run many sessions under one daemon, watch them all, and manage them via the control plane |
| [How to avoid Copilot provider pitfalls](copilot-provider-pitfalls.md) | Diagnose why GitHub Copilot shows fewer models than expected and pick the right OAuth client/token type |
