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
| `resume [id]` | Resume a hosted session (picker when no id is given) |
| `attach [id]` | Attach the TUI to a hosted session |
| `run <prompt>` / `exec <prompt>` | Headless non-interactive execution with prompt |
| `auth <list\|show\|set>` | Manage model provider authentication & API keys |
| `config <path\|list\|get\|set>` | Inspect or mutate `config.toml` |
| `mcp <list>` | Inspect configured MCP servers |
| `skill <list>` | Discover and inspect available skills |
| `session <list\|attach\|delete\|dashboard>` | Session management commands |
| `daemon <start\|stop\|status>` | Manage the unified background session daemon |
| `serve [--port <n>] [--public] [--detach] [--idle-exit <min>] [--grace <secs>]` | Run the session daemon (foreground; `--detach` backgrounds it) |
| `stop` | Stop the running daemon gracefully |
| `status [--watch] [--json] [--all]` | Show the daemon's sessions needing attention |
| `dashboard` | Open the full-screen session dashboard |
| `doctor` | Verify stored session integrity |
| `completions <bash\|zsh\|fish>` | Print a shell completion script |
| `help [command]` | Print top-level or command help |

`neenee showcase <name>` (render one UI component standalone) exists only in
debug builds and is listed only there.

| Option | Effect |
|--------|--------|
| `--project <path>` | Operate on the project at `<path>` |
| `--autopilot` | Run without confirmations or questions this session |
| `--json` | Machine-readable JSON output for status or headless run |
| `-h`, `--help` | Print help (`neenee help <command>` for a command's help) |
| `-V`, `--version` | Print the version and exit |

## Conventions

- `--help` and `--version` print to **stdout** and exit **0**; either one
  short-circuits every other mode before any session, lock, or network work.
- Misuse (unknown command or option, missing or invalid value) prints a
  short error plus a `--help` pointer to **stderr** and exits **2**.
- An unrecognized command close to a real one earns a `tip: a similar
  command exists: '…'` line.
- `--attach [id]` is the legacy flag form of `neenee attach`; it still
  parses but is not advertised in help.

## The daemon

`neenee serve` runs the session daemon (normally spawned on demand by
`neenee` itself; run it explicitly — including under a supervisor — with
`neenee serve --detach`). It accepts `--port <n>`, `--public`,
`--idle-exit <min>`, `--grace <secs>`, `-h`/`--help`, and `-V`/`--version`,
with the same exit-code conventions. Since ADR-0102 there is no separate
server binary: `neenee` is the one executable. (There is no `--project`:
the daemon is project-agnostic since the unified model, and the old flag
did nothing.)

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
| `NEENEE_LOG` | Log level for the file log under the XDG state dir: `off`, `error`, `warn`, `info` (default), `debug`, `trace` |
| `RUST_LOG` | Per-target filter; takes precedence over `NEENEE_LOG` when set |
