# Command line

The `neenee` command line: subcommands, options, and exit behavior. Slash
commands typed *inside* the TUI are covered under
[Slash commands](commands.md).

Every interactive invocation is a client of the per-user **session daemon**,
spawned on demand when missing; see
[How to track sessions with a session daemon](../how-to/track-sessions-with-a-session-daemon.md).

## Usage

```text
neenee [OPTIONS] [COMMAND]
```

| Command | Effect |
|---------|--------|
| *(none)* | Start a fresh session hosted by the daemon |
| `attach [id]` | Attach the TUI to a hosted session — the TUI picker opens when no id is given |
| `run <prompt>` | Headless non-interactive execution with the prompt |
| `auth <list\|show\|set>` | Manage model provider authentication & API keys |
| `config <path\|list\|get\|set\|check>` | Inspect or mutate `config.toml`; `check` validates it (syntax errors, typo'd keys, dead legacy spellings) |
| `mcp ls` | List configured MCP servers (the bare `neenee mcp` teaches the subcommand) |
| `skill ls` | List discovered skills (the bare `neenee skill` teaches the subcommand) |
| `session rm <id>` | Terminate a hosted session by id — listing is `daemon status`, the daemon's view of what it hosts |
| `daemon start [--fg] [--port <n>] [--public] [--idle-exit <min>] [--grace <secs>]` | Start the session daemon (detached by default; `--fg` stays in the foreground for supervisors) |
| `daemon stop` | Stop the running daemon gracefully (budget-coordinated, ADR-0119) |
| `daemon status [--watch] [--json] [--all] [--diagnostic]` | Show the daemon's sessions and endpoint health |
| `dashboard` | Open the full-screen session dashboard. Leaving it (`Esc`, or `Ctrl+C` twice) exits the TUI entirely — no conversation is left behind |
| `panel [url\|open]` | The web panel's address for the running daemon: `url` (the default, and the bare form) prints it with its token; `open` also launches the platform browser (`$BROWSER`, else xdg-open/open) |
| `doctor` | Verify stored session integrity |
| `completions <bash\|zsh\|fish>` | Print a shell completion script |
| `help [command]` | Print top-level or command help |

`neenee showcase <name>` (render one UI component standalone) exists only in
debug builds and is listed only there; the component playground is
[Showcase](../dev/showcase.md) in the contributor docs.

| Option | Effect |
|--------|--------|
| `--project <path>` | Operate on the project at `<path>` |
| `--home <dir>` | Run as a fully separate instance rooted at `<dir>/neenee/` — config, data, daemon files, and (with `NEENEE_PORT`) the port; the CLI form of `NEENEE_HOME` (ADR-0121) |
| `--remote <addr>` | Run headless against an explicitly named daemon (`host:port` or `ws://host:port`) instead of the local instance — no discovery, no spawn |
| `--token <token>` | The bearer token `--remote` requires (every network-exposed daemon demands one; see `panel url` on the host) |
| `-p`, `--prompt`, `--print <text>` | Run `<text>` as a headless one-shot (non-interactive) |
| `-i`, `--interactive` | Force the interactive TUI even when a `-p` prompt is given |
| `-y`, `--yolo` | Alias for `--autopilot`: run without confirmations or questions |
| `--autopilot` | Run without confirmations or questions this session |
| `-j`, `--json` | Machine-readable JSON output for status or headless run |
| `--config-dir <dir>` / `--data-dir <dir>` / `--state-dir <dir>` / `--cache-dir <dir>` | Per-category XDG overrides (ADR-0014 §3): the CLI form of each `NEENEE_*_DIR` env var; a category flag wins over `--home` for its own category |
| `-h`, `--help` | Print help (`neenee help <command>` for a command's help) |
| `-V`, `--version` | Print the version and exit |

## Conventions

- `--help` and `--version` print to **stdout** and exit **0**; either one
  short-circuits every other mode before any session, lock, or network work.
- Misuse (unknown command or option, missing or invalid value) prints a
  short error plus a `--help` pointer to **stderr** and exits **2**.
- An unrecognized command close to a real one earns a `tip: a similar
  command exists: '…'` line.
- Retired spellings (`serve`, `stop`, `status`, `resume`, `exec`) are
  removed outright (ADR-0135): a retired word is an unrecognized command
  (exit 2), like any other typo — no alias and no redirect.
- `--attach [id]` normalizes to `neenee attach [id]`.

## The daemon

`neenee daemon start` runs the session daemon (normally spawned on demand
by `neenee` itself). It **detaches by default** — the verb asks for a
daemon — and takes `--fg` for supervisors (systemd/tmux keep the process
in the foreground and provide their own daemonization); `--no-local-auth`,
`--port <n>`, `--public`, `--idle-exit <min>`, and `--grace <secs>` apply
to both shapes. Since ADR-0102 there is no separate server binary: `neenee`
is the one executable. (There is no `--project`: the daemon is
project-agnostic since the unified model.)

The daemon stops gracefully on SIGINT, SIGTERM, or SIGHUP within its grace
budget (`[daemon] shutdown_grace_secs`, default 10s): it stops accepting,
closes live connections, fires every hosted session's `SessionEnd` hooks
(each under its own deadline), removes its discovery record, and exits 0.
A second signal — or the budget expiring — skips the remaining wait and
forces the exit (still 0). It exits on its own after
`[daemon] idle_exit_minutes` (default 5) of hosting zero sessions with zero
attached clients; `0` disables that (see
[the systemd unit](https://github.com/ming2k/neenee/blob/main/assets/neenee.service)
for an always-on deployment).

## Shell completions

`neenee completions <shell>` prints a static completion script:

```bash
# bash
eval "$(neenee completions bash)"
# zsh
neenee completions zsh > "${fpath[1]}/_neenee"
# fish
neenee completions fish > ~/.config/fish/completions/neenee.fish
```

## Environment

| Variable | Effect |
|----------|--------|
| `NEENEE_HOME` | Instance root (ADR-0121): redirects config, data, state, cache, the daemon's runtime files, and (with `NEENEE_PORT`) the default port under one root — the env form of `--home`. Must be absolute; relative values are ignored |
| `NEENEE_PORT` | Default TCP port for `daemon start` when `--port` is absent (overrides the well-known 9800) |
| `NEENEE_CONFIG_DIR`, `NEENEE_DATA_DIR`, `NEENEE_STATE_DIR`, `NEENEE_CACHE_DIR` | Per-category directory overrides (see [Paths](paths.md)) |
| `NEENEE_LOG` | Log level for the file log under the XDG state dir: `off`, `error`, `warn`, `info` (default), `debug`, `trace` |
| `RUST_LOG` | Per-target filter; takes precedence over `NEENEE_LOG` when set |
