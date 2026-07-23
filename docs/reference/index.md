# Reference

Lookup-oriented documentation — tables, lists, and exact values.

## Tools and providers

- [Built-in tools](tools/) — tool catalog, access tiers, capability axes, and
  per-tool parameter schemas (one page per tool category)
- [Providers](providers.md) — capability matrix, endpoint and env var catalog
- [Model metadata](model-metadata.md) — static fallback, trusted remote metadata, and model discovery precedence

## Commands

- [Slash commands](commands.md) — built-in commands, subcommands, custom commands

## Configuration

- [Configuration](configuration.md) — every `config.toml` key with its default

## Server API

- [WebSocket frontend integration](server-api.md) — connection lifecycle,
  message framing, frontend flows, and compatibility guidance
- [AsyncAPI contract](server.asyncapi.yaml) — machine-readable WebSocket
  channels, operations, schemas, and examples

## Files and persistence

- [Paths](paths.md) — every file neenee reads or writes, by XDG category,
  with override precedence and cleanup quick reference

## TUI

- [TUI overview](tui/) — component map, file responsibilities
- [Frame layout](tui/layout.md) — vertical chunks, chrome hiding, measurements
- [Color palette](tui/theme.md) — all theme tokens with RGB values
- [Half-block characters](tui/half-block-chars.md) — `╻╹▀▄┃` Unicode reference

### Components

| Component | File |
|-----------|------|
| User message | [user-message.md](tui/user-message.md) |
| Input box | [input-box.md](tui/input-box.md) |
| Assistant text | [assistant-text.md](tui/assistant-text.md) |
| Code block | [code-block.md](tui/code-block.md) |
| Expandable step | [expandable-step.md](tui/expandable-step.md) |
| Tool step | [tool-step.md](tui/tool-step.md) |
| Thinking step | [thinking-step.md](tui/thinking-step.md) |
| Step state machine | [step-state.md](tui/step-state.md) |
| Envoy view | [envoy-view.md](tui/envoy-view.md) |
| Activity bar | [status-bar.md](tui/status-bar.md) |
| Hint bar | [hint-line.md](tui/hint-line.md) |
| Modals | [modals.md](tui/modals.md) |
