# Command lines

Muta has two command surfaces: `muta` for daemon and service control, and
`mutx` for the terminal app. Slash commands typed *inside* the TUI are under
[Slash commands](commands.md).

Every `mutx` session is a client of the per-user daemon. A local invocation
starts `muta` on demand when the daemon is missing; see
[How to track sessions with a session daemon](../how-to/track-sessions-with-a-session-daemon.md).

## Core usage

```text
muta [OPTIONS] <COMMAND>
```

| Command | Effect |
|---------|--------|
| `auth <list\|show\|set>` | Manage model provider authentication & API keys |
| `config <path\|list\|get\|set\|check>` | Inspect or mutate `config.toml`; `check` validates it (syntax errors, typo'd keys, dead legacy spellings) |
| `mcp ls` | List configured MCP servers (the bare `muta mcp` teaches the subcommand) |
| `mcp add <name> -- <cmd> [args…]` | Register a stdio MCP server in the user config; flags: `--env K=V`, `--read-only`, `--disabled`, `--allow-tools`, `--deny-tools` |
| `mcp add <name> --url <endpoint>` | Register a Streamable HTTP MCP server |
| `mcp rm <name>` | Remove a server from the user config |
| `mcp enable <name>` / `mcp disable <name>` | Toggle a server without removing it |
| `mcp get <name>` | Print one server's config entry |
| `mcp probe <name>` | Connect once, list the advertised tools, disconnect |
| `mcp import (- \| <file>)` | Merge `[mcp.*]` TOML into the user config, e.g. `aegis-mcp print-config \| muta mcp import -` |
| `skill ls` | List discovered skills (the bare `muta skill` teaches the subcommand) |
| `session rm <id>` | Terminate a hosted session by id — listing is `daemon status`, the daemon's view of what it hosts |
| `daemon start [--fg] [--port <n>] [--public] [--idle-exit <min>] [--grace <secs>]` | Start the session daemon (detached by default; `--fg` stays in the foreground for supervisors) |
| `daemon stop` | Stop the running daemon gracefully (budget-coordinated, ADR-0119) |
| `daemon status [--watch] [--json] [--all] [--diagnostic]` | Show the daemon's sessions and endpoint health |
| `daemon token` | Print the local TCP bearer token on explicit operator request; prints a diagnostic instead when authentication is disabled |
| `doctor` | Verify stored session integrity |
| `completions <bash\|zsh\|fish>` | Print a shell completion script |
| `help [command]` | Print top-level or command help |

With no command, `muta` prints core help. It does not open a terminal session.

## Terminal app usage

```text
mutx [OPTIONS] [PROMPT]
mutx [OPTIONS] <COMMAND>
```

| Command | Effect |
|---------|--------|
| *(none)* | Start a fresh session hosted by the daemon |
| `attach [id]` | Attach the TUI to a hosted session; the picker opens when no id is given |
| `run <prompt>` | Headless non-interactive execution with the prompt |
| `dashboard` | Open the full-screen session dashboard |
| `completions <bash\|zsh\|fish>` | Print a `mutx` shell completion script |
| `help [command]` | Print top-level or command help |

`mutx showcase <name>` renders one UI component in debug builds only. The
component playground is
[Showcase](../dev/showcase.md) in the contributor docs.

## Options

| Option | Surface | Effect |
|--------|---------|--------|
| `--project <path>` | Both | Operate on the project at `<path>` |
| `--home <dir>` | Both | Use the isolated instance rooted at `<dir>/muta/`; the CLI form of `MUTA_HOME` |
| `--config-dir <dir>` / `--data-dir <dir>` / `--state-dir <dir>` / `--cache-dir <dir>` | Both | Override one XDG category; the category flag wins over `--home` |
| `--remote <addr>` | `mutx` | Run headless against `host:port` or `ws://host:port`; no local discovery or spawn |
| `--token <token>` | `mutx` | Supply the bearer token required by `--remote` |
| `-p`, `--prompt`, `--print <text>` | `mutx` | Run `<text>` as a headless one-shot |
| `-i`, `--interactive` | `mutx` | Force the TUI even when a `-p` prompt is given |
| `--delegate`, `--auto`, `-y`, `--yolo`, `--autopilot` | `mutx` | Run in delegated autonomous mode (without confirmations or questions) |
| `-j`, `--json` | Both | Emit machine-readable output where supported |
| `-h`, `--help` | Both | Print help |
| `-V`, `--version` | Both | Print that binary's version and exit |

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
- `--attach [id]` normalizes to `mutx attach [id]`.
- A `muta`-only command passed to `mutx` exits 2 and points to `muta`.
- A TUI command passed to `muta` is an unrecognized command.

## The daemon

`muta daemon start` runs the session daemon (normally spawned on demand
by `mutx`). It **detaches by default** — the verb asks for a
daemon — and takes `--fg` for supervisors (systemd/tmux keep the process
in the foreground and provide their own daemonization); `--no-local-auth`,
`--port <n>`, `--public`, `--idle-exit <min>`, and `--grace <secs>` apply
to both shapes. The daemon implementation exists only in `muta`; `mutx` is a
client and cannot enter daemon mode. (There is no `--project`: the daemon is
project-agnostic since the unified model.)

The daemon stops gracefully on SIGINT, SIGTERM, or SIGHUP within its grace
budget (`[daemon] shutdown_grace_secs`, default 10s): it stops accepting,
closes live connections, fires every hosted session's `SessionEnd` hooks
(each under its own deadline), removes its discovery record, and exits 0.
A second signal — or the budget expiring — skips the remaining wait and
forces the exit (still 0). It exits on its own after
`[daemon] idle_exit_minutes` (default 5) of hosting zero sessions with zero
attached clients; `0` disables that (see
[the systemd unit](https://github.com/ming2k/muta/blob/main/assets/muta.service)
for an always-on deployment).

The daemon's TCP port serves the WebSocket control plane and the generic
`GET /healthz` probe. It does not embed or serve either frontend. Build and
host `apps/web` independently, then use `muta daemon token` to configure its
authenticated connection.

## Shell completions

Both binaries print their own static completion script:

```bash
# bash
eval "$(muta completions bash)"
# zsh
muta completions zsh > "${fpath[1]}/_muta"
# fish
muta completions fish > ~/.config/fish/completions/muta.fish

# terminal app (replace bash with zsh or fish as needed)
eval "$(mutx completions bash)"
```

## Environment

| Variable | Effect |
|----------|--------|
| `MUTA_HOME` | Instance root (ADR-0121): redirects config, data, state, cache, the daemon's runtime files, and (with `MUTA_PORT`) the default port under one root — the env form of `--home`. Must be absolute; relative values are ignored |
| `MUTA_PORT` | Default TCP port for `daemon start` when `--port` is absent (overrides the well-known 9800) |
| `MUTA_CONFIG_DIR`, `MUTA_DATA_DIR`, `MUTA_STATE_DIR`, `MUTA_CACHE_DIR` | Per-category directory overrides (see [Paths](paths.md)) |
| `MUTA_LOG` | Log level for the file log under the XDG state dir: `off`, `error`, `warn`, `info` (default), `debug`, `trace` |
| `MUTA_BIN` | Explicit `muta` executable used by `mutx` for on-demand daemon startup; normally unnecessary because a sibling binary and then `PATH` are checked |
| `RUST_LOG` | Per-target filter; takes precedence over `MUTA_LOG` when set |
