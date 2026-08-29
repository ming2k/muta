<p align="center">
  <img src="./assets/logo.png" alt="muta logo" width="256">
</p>

<h1 align="center">muta</h1>

<p align="center">
  English | <a href="./README.zh-CN.md">简体中文</a>
</p>

<p align="center">
  A high-performance, controllable AI Harness for software engineering — featuring a semantic TUI, layered execution control, background session daemon, and autonomous tool orchestration.
</p>

<p align="center">
  <a href="#"><img src="https://img.shields.io/badge/rust-2024-orange?logo=rust" alt="Rust 2024"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue" alt="License"></a>
</p>

## Highlights

- **AI Harness & Controllability** — A robust control plane around LLM execution with strict deterministic boundaries, guardrails, non-interactive discipline, and context lifecycle management.
- **Semantic TUI** — Custom high-performance terminal UI with live progress indicators, collapsible tool steps, and syntax-aware diffs.
- **Autonomous Tool Orchestration** — ReAct execution loop with PTY shell control, file operations, codebase indexing, web search, and MCP (Model Context Protocol) integration.
- **Session Daemon** — Background user-level daemon manages long-running sessions across projects. Detach, close the terminal, or switch between tasks without interrupting work.
- **Scheduled Prompts & Automation** — Automate recurring tasks with cron-style schedules or one-shot countdown timers via `/schedule`.
- **Durable Sessions & Compaction** — Persistent conversation history with atomic storage, split context compaction, branching, and instant resume.
- **Skills & Extensibility** — Load domain-specific instructions, workflows, and tools on demand or automatically by mention.

## Quick Start

### Install Prebuilt Binary

**macOS & Linux**:

```bash
curl -fsSL https://raw.githubusercontent.com/ming2k/muta/main/install.sh | bash
```

**Windows (PowerShell)**:

```powershell
irm https://raw.githubusercontent.com/ming2k/muta/main/install.ps1 | iex
```

### Build from Source

```bash
git clone https://github.com/ming2k/muta.git
cd muta
cargo build --release -p muta -p mutx
```

### Getting Started

1. Launch the TUI client:
   ```bash
   mutx
   ```
2. Configure your model provider:
   Type `/models` in the prompt box to pick a provider and enter your API key.
3. Start coding. Press `F1` at any time inside the TUI for keyboard shortcuts and help.

## Documentation

- [Getting Started & How-to Guides](docs/how-to/) — Setup, configuration, and everyday workflows.
- [Architecture & Design](docs/explanation/) — Daemon architecture, rendering pipeline, and state model.
- [Reference](docs/reference/) — CLI commands, slash commands, configuration schema, and API specs.
- [Architecture Decision Records (ADRs)](docs/adr/) — Design choices and technical specifications.

## License

[MIT](LICENSE)
