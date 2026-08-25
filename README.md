<p align="center">
  <img src="./assets/logo.png" alt="muta logo" width="256">
</p>

<h1 align="center">muta</h1>

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

- **Semantic TUI** — In-house grid + diff rendering engine (`mutx-engine`), built from scratch to replace ratatui. Retained-mode grid with write-marks-dirty diff, wide-glyph ownership, and `bce`-aware crossterm backend. Live status, expandable tool steps, and structured diffs.
- **Tool Use** — Full ReAct loop with native and fallback tool-calling; bash, file I/O, grep, glob, web search, and MCP servers.
- **Scheduled Prompts** — Schedule prompts on a clock with `/schedule`: recurring cron jobs or one-shot countdown/absolute-time timers, so the agent can run on autopilot on a schedule.
- **Session Daemon & Control Plane** — The `muta` core daemon owns every session across every project, while `mutx` and the web app are peer clients. Work survives closed terminals; `muta daemon status` provides a live multi-task view and `/dashboard` in `mutx` switches sessions without killing them.
- **Durable Sessions** — Atomic persistence with compaction, resume, and fork.
- **Skills** — Load domain-specific instructions on demand or automatically by mention.

## Quick Start

**Install in one line** on macOS or Linux — downloads and SHA-256 verifies a prebuilt binary into `~/.local/bin`:

```bash
curl -fsSL https://raw.githubusercontent.com/ming2k/muta/main/install.sh | bash
```

> Pin this release with `MUTA_VERSION=0.33.1`, or install into a custom dir with `INSTALL_DIR=/usr/local/bin`.

On Windows (PowerShell), install the verified release build for the current user:

```powershell
irm https://raw.githubusercontent.com/ming2k/muta/main/install.ps1 | iex
```

The Windows installer supports x86-64, verifies the release SHA-256, installs under `%LOCALAPPDATA%\Programs\muta\bin`, and adds that directory to the user `PATH`. Override it with `MUTA_INSTALL_DIR`.

**Or build from source**:

```bash
git clone https://github.com/ming2k/muta.git
cd muta
cargo build --release -p muta -p mutx
cargo run --release -p mutx
```

On first launch, type `/models` to pick a model and enter your API key (`Ctrl+M` works too, where the Kitty keyboard protocol is active). Then just start typing.

The first `mutx` checks the Muta daemon and starts the sibling `muta` binary
when needed. Later launches attach immediately. See
[Daemon mode](#daemon-mode-and-multi-session-tracking) below.

## Daemon mode and multi-session tracking

The `muta` core runs one user-level **session daemon** that owns every session
across every project (ADR-0096). The `mutx` terminal app and the web app are
independent clients. Sessions keep running without either frontend:

```bash
mutx                   # open the TUI; auto-start muta when needed
muta daemon start      # run the daemon (detached by default)

muta daemon start --fg --public  # foreground, all interfaces (TCP+token), for LAN clients
mutx attach [id]       # drive a specific daemon-held session
muta daemon status     # one-shot table: sessions needing attention
muta daemon status --watch    # live table, redraws on every change
muta daemon status --json     # raw monitor frames (the control-panel API)
mutx dashboard         # the full-screen dashboard, straight from the shell
```

Inside the TUI, **`/dashboard`** opens the session dashboard: a full-screen
live view over every daemon session — a console region (the selected
session's live status: current tool, activity, context, progress) over a
sessions dock with one card per session. Enter opens a read-only preview; `a`
attaches to a hosted session — the TUI detaches and re-attaches, so the
session you leave **keeps running** in the daemon. From the same surface you
can interrupt (`i`), prompt (`p`), or create (`n`) a session. Closing the TUI
never ends a round; re-attach any time with `mutx attach <id>`. (`/host` is
kept as a hidden alias.)

**`mutx dashboard`** reaches that same full-screen dashboard straight from
the shell — no need to enter a session first. It attaches to the daemon's
most-recently-active session only as the underlying carrier and raises the
dashboard over it: Esc quits, `a` on a card attaches into that session. Like
`mutx dashboard` performs the normal daemon readiness check, but it still
needs at least one existing session as its carrier.

The daemon speaks one read/write control-plane protocol (create, prompt,
interrupt, approve, kill, plus the monitor stream) over a Unix socket on
macOS/Linux or a current-user-only Windows Named Pipe, and over TCP+token when
exposed — which is what a web control panel consumes directly. See
[How to track sessions with a session daemon](docs/how-to/track-sessions-with-a-session-daemon.md)
and [ADR-0096](docs/adr/0096-unified-session-daemon.md).

## Key Bindings

| Key | Action |
|-----|--------|
| `F1` | Help (all keybindings) |
| `F5` | `/btw` asides list |
| `Ctrl+Q` | Open the round queue |
| `Ctrl+P` | Block / resume the round queue |
| `Ctrl+O` | Insert input into the running round |
| `Ctrl+M` | Open the model picker (Kitty keyboard protocol; `/models` always works) |
| `Ctrl+L` | Global view switcher — jump between every surface (pickers, dashboard, sessions included), MRU-first; typing filters fuzzily. Views are retained: leaving one keeps its scroll, selection, and (for pickers) your in-progress composer draft (ADR-0133) |
| `Ctrl+R` | Input history search |
| `Ctrl+T` | Open todos |
| `Enter` | Send message |
| `Tab` | Commit the highlighted slash-command / `@path` completion (re-opens a menu `Esc` dismissed) |
| `Ctrl+B` | Move the caret back one character (readline backward-char) |
| `Ctrl+C` | Copy → interrupt → close modal → clear → quit |
| `Ctrl+V` | Paste from clipboard |

The full, authoritative list lives in the TUI itself: press `F1` for Help.

## Useful Commands

| Command | Description |
|---------|-------------|
| `/schedule <when> <prompt>` | Schedule a prompt on a cron (recurring) or a countdown/absolute time (one-shot) |
| `/compact` | Compact context to free up space |
| `/sessions` | Browse and open past sessions |
| `/usage` | Cross-session usage statistics: daily tokens, per-model totals, recent request log (survives session cleanup) |
| `/export` | Export conversation as Markdown |
| `/mcp` | Inspect MCP server connections |

See [docs/](docs/) for architecture, guides, and reference.

## License

[MIT](LICENSE)
