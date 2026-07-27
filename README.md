<p align="center">
  <img src="./assets/logo.png" alt="neenee logo" width="256">
</p>

<h1 align="center">neenee</h1>

<p align="center">
  English | <a href="./README.zh-CN.md">简体中文</a>
</p>

<p align="center">
  A Rust-based AI coding agent with a semantic TUI, tool use, on-demand skills, and scheduled prompts.
</p>

<p align="center">
  <a href="#"><img src="https://img.shields.io/badge/rust-2024-orange?logo=rust" alt="Rust 2024"></a>
  <a href="#"><img src="https://img.shields.io/badge/license-MIT-blue" alt="License"></a>
</p>

## Features

- **Semantic TUI** — In-house grid + diff rendering engine (`neenee-tui-engine`), built from scratch to replace ratatui. Retained-mode grid with write-marks-dirty diff, wide-glyph ownership, and `bce`-aware crossterm backend. Live status, expandable tool steps, and structured diffs.
- **Tool Use** — Full ReAct loop with native and fallback tool-calling; bash, file I/O, grep, glob, web search, and MCP servers.
- **Scheduled Prompts** — Schedule recurring prompts on a clock with `/repeat` so the agent can run unattended on a schedule.
- **Durable Sessions** — Atomic persistence with compaction, resume, and fork.
- **Skills** — Load domain-specific instructions on demand or automatically by mention.

## Quick Start

**Install in one line** (macOS & Linux) — downloads a prebuilt binary into `~/.local/bin`:

```bash
curl -fsSL https://raw.githubusercontent.com/ming2k/neenee/main/install.sh | bash
```

> Pin a version with `NEENEE_VERSION=0.10.0`, or install into a custom dir with `INSTALL_DIR=/usr/local/bin`.

**Or build from source**:

```bash
git clone https://github.com/ming2k/neenee.git
cd neenee
cargo run --release
```

On first launch, press `Ctrl+M` to pick a model and enter your API key. Then just start typing.

## Key Bindings

| Key | Action |
|-----|--------|
| `Enter` | Send message |
| `Tab` | Accept slash-command / `@path` completion |
| `Ctrl+M` | Open the model picker |
| `Ctrl+T` | Open todos |
| `Ctrl+B` | Toggle between input and conversation stream |
| `Ctrl+C` | Copy → interrupt → close modal → clear → quit |
| `Ctrl+V` | Paste from clipboard |

## Useful Commands

| Command | Description |
|---------|-------------|
| `/repeat <cron> <prompt>` | Schedule a prompt on a cron expression |
| `/compact` | Compact context to free up space |
| `/session list` | Browse and resume past sessions |
| `/export` | Export conversation as Markdown |
| `/mcp` | Inspect MCP server connections |

See [docs/](docs/) for architecture, guides, and reference.

## License

[MIT](LICENSE)
