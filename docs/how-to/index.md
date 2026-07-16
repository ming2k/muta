# How-to guides

Task-oriented guides for extending neenee. Each guide assumes familiarity
with the relevant reference material.

| Guide | Task |
|-------|------|
| [How to add a built-in tool](add-a-tool.md) | Implement the `Tool` trait, pick a `ToolAccess`, register, verify |
| [How to add a provider](add-a-provider.md) | Wrap `OpenAiCompatProvider` or build a standalone adapter, register dispatch sites |
| [How to ask the user a question during a task](ask-the-user.md) | Use `ask_user` to resolve ambiguity or collect preferences mid-task |
| [How to configure TUI appearance](configure-tui-appearance.md) | Apply a color preset or edit a custom semantic palette from `/config` |
| [How to enable the live quant broker](enable-live-quant-broker.md) | Connect `neenee-quant` directly to LongPort OpenAPI with local risk checks |
| [How to use the intelligence workbench](use-intelligence-workbench.md) | Collect public signals, track link changes, and convene the expert council |
| [How to use sub2api relays](use-sub2api.md) | Configure OpenAI, Anthropic, and Gemini-compatible sub2api relays |
