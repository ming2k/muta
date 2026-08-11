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
- **Scheduled Prompts** — Schedule prompts on a clock with `/schedule`: recurring cron jobs or one-shot countdown/absolute-time timers, so the agent can run on autopilot on a schedule.
- **Session Daemon & Control Plane** — One user-level daemon owns every session across every project, so work survives closed terminals and you can watch or drive any of it from anywhere: `neenee status` for a live multi-task view, `/dashboard` in the TUI to switch sessions without killing them, and a read/write control API (create / prompt / interrupt / approve / kill) over a local socket or a token-protected LAN port — the same protocol a web panel consumes.
- **Durable Sessions** — Atomic persistence with compaction, resume, and fork.
- **Skills** — Load domain-specific instructions on demand or automatically by mention.

## Quick Start

**Install in one line** (macOS & Linux) — downloads a prebuilt binary into `~/.local/bin`:

```bash
curl -fsSL https://raw.githubusercontent.com/ming2k/neenee/main/install.sh | bash
```

> Pin a version with `NEENEE_VERSION=0.22.1`, or install into a custom dir with `INSTALL_DIR=/usr/local/bin`.

**Or build from source**:

```bash
git clone https://github.com/ming2k/neenee.git
cd neenee
cargo run --release
```

On first launch, press `Ctrl+M` to pick a model and enter your API key. Then just start typing.

The first `neenee` spawns the session daemon (a one-time cold start; every later launch attaches instantly). See [Daemon mode](#daemon-mode-and-multi-session-tracking) below.

## Daemon mode and multi-session tracking

neenee runs as a client of one user-level **session daemon** that owns every
session across every project (ADR-0096). Sessions keep running without a TUI,
and several clients can co-drive or observe them:

```bash
neenee                   # attach to the daemon (auto-started on first use)
neenee serve             # run the daemon in the foreground
neenee serve --detach    # ... or in the background
neenee serve --expose    # also listen on TCP+token for LAN clients
neenee attach [id]       # drive a specific daemon-held session
neenee status            # one-shot table: sessions needing attention
neenee status --watch    # live table, redraws on every change
neenee status --json     # raw monitor frames (the control-panel API)
neenee dashboard         # the full-screen dashboard, straight from the shell
```

Inside the TUI, **`/dashboard`** opens the session dashboard: a full-screen
live view over every daemon session — a session list plus a detail pane
(current tool, activity, context, progress) for the selected row. Enter
attaches to a hosted session — the TUI detaches and re-attaches, so the
session you leave **keeps running** in the daemon. From the same surface you
can interrupt (`i`), prompt (`p`), or create (`n`) a session. Closing the TUI
never ends a round; re-attach any time with `neenee attach <id>`. (`/host` is
kept as a hidden alias.)

**`neenee dashboard`** reaches that same full-screen dashboard straight from
the shell — no need to enter a session first. It attaches to the daemon's
most-recently-active session only as the underlying carrier and raises the
dashboard over it: Esc quits, Enter on a row attaches into that session. Like
`neenee status` it never spawns a daemon, so it needs a running host with at
least one session.

The daemon speaks one read/write control-plane protocol (create, prompt,
interrupt, approve, kill, plus the monitor stream) over a Unix socket by
default and over TCP+token when exposed — which is what a web control panel
consumes directly. See
[How to track sessions with a session host](docs/how-to/track-sessions-with-a-session-host.md)
and [ADR-0096](docs/adr/0096-unified-session-daemon.md).

## Key Bindings

| Key | Action |
|-----|--------|
| `Enter` | Send message |
| `Tab` | Accept slash-command / `@path` completion |
| `Ctrl+M` | Open the model picker |
| `Ctrl+T` | Open todos |
| `Ctrl+B` | Move the caret back one character (readline backward-char) |
| `Ctrl+C` | Copy → interrupt → close modal → clear → quit |
| `Ctrl+V` | Paste from clipboard |

## Useful Commands

| Command | Description |
|---------|-------------|
| `/schedule <when> <prompt>` | Schedule a prompt on a cron (recurring) or a countdown/absolute time (one-shot) |
| `/compact` | Compact context to free up space |
| `/session list` | Browse and resume past sessions |
| `/export` | Export conversation as Markdown |
| `/mcp` | Inspect MCP server connections |

See [docs/](docs/) for architecture, guides, and reference.

## License

[MIT](LICENSE)
