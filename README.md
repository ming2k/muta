<p align="center">
  <img src="./assets/logo.png" alt="muta logo" width="256">
</p>

<h1 align="center">muta</h1>

<p align="center">
  English | <a href="./README.zh-CN.md">简体中文</a>
</p>

<p align="center">
  A universal, security-first AI agent harness — featuring flexible in-framework extensibility, multi-model interfaces with comprehensive mainstream capabilities, and extensible frontends across terminal, web, and headless runtimes.
</p>

<p align="center">
  <a href="#"><img src="https://img.shields.io/badge/rust-2024-orange?logo=rust" alt="Rust 2024"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue" alt="License"></a>
</p>

## Highlights

- **Universal Agent Purpose** — Beyond a simple code editor; a general-purpose agent harness supporting cognitive research, software engineering, and custom personas with hermetic, symmetric role definitions.
- **Flexible In-Framework Extensibility** — Modular capability expansion through self-contained role bundles, on-demand skills, lifecycle hooks, and Model Context Protocol (MCP) integrations.
- **Multi-Model Provider Support** — Native, first-class adapters for Anthropic, OpenAI, Gemini, DeepSeek, and custom OpenAI-compatible endpoints with dynamic, role-level model routing.
- **Comprehensive Model Capabilities** — Complete frontier capability coverage: structured tool calling, reasoning/thinking tiers, multimodal vision, live streaming, and deterministic prompt caching.
- **Extensible Frontends** — High-performance semantic terminal TUI (`mutx`), reactive Web interface, and background session daemon decoupled through strongly-typed contracts.
- **Security-First & Trust Architecture** — Three orthogonal security planes: cryptographic file asset attestation with 30-day bounded leases, strict user-owned spatial boundaries, and a four-tier runtime hazard mesh guarding against indirect prompt injection.

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
3. Start interacting. Press `Ctrl+P` (or type `/`) at any time inside the TUI to discover commands and shortcuts.

## Documentation

- [Getting Started & How-to Guides](docs/how-to/) — Setup, configuration, and everyday workflows.
- [Architecture & Design](docs/explanation/) — Daemon architecture, rendering pipeline, and state model.
- [Reference](docs/reference/) — CLI commands, slash commands, configuration schema, and API specs.
- [Architecture Decision Records (ADRs)](docs/adr/) — Design choices and technical specifications.

## License

[MIT](LICENSE)

---

<details>
<summary><b>About the Name & Mascot</b></summary>

- **muta** originates from the Indo-European root *\*mut-* (signifying "change" and "growth"), reflecting that AI is an evolving, continually growing entity.
- In Chinese, *muta* sounds like **沐獭** (*Mù Tǎ*, literally "bathing otter"), which inspired our mascot: an otter taking a bath 🦦🛁.

</details>
