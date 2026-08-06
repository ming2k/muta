# Slash commands

Built-in commands typed in the input box. The descriptions in this table are
the canonical source of truth and match the slash-suggestion popup and the
`/help` output exactly.

Project and user-defined commands are covered under
[Custom commands](#custom-commands).

## Built-in commands

| Command | Description |
|---------|-------------|
| `/models` | Switch the active model |
| `/connections` | Manage LLM provider connections |
| `/mcp` | Manage MCP servers (enable/disable, reconnect) |
| `/compact` | Compact older complete rounds now |
| `/clear` | Clear the conversation history |
| `/permissions [clear]` | Show or clear always-allowed tool rules |
| `/autopilot [on\|off]` | Toggle autopilot mode (agent runs without human intervention) |
| `/principal <code\|architect\|reviewer\|security>` | Switch the principal role — changes persona and capability scope |
| `/review` | Run an on-demand session-review diagnostic of the current round |
| `/search <query>` | Semantic search over the project's session history |
| `/session [status\|list\|resume\|fork\|open\|new]` | Manage durable sessions |
| `/sessions` | Browse past sessions |
| `/btw` | Open a side conversation that runs alongside the main session |
| `/resume [id]` | Resume the most recent or selected session |
| `/repeat [cron prompt\|list\|cancel id]` | Schedule a prompt on a cron expression (cron-only alias for `/schedule`) |
| `/schedule [when prompt\|list\|cancel id]` | Schedule a prompt: cron (recurring) or countdown/absolute-time (one-shot) |
| `/init [path]` | Initialize a `.neenee/` config tree |
| `/reload` | Re-read config.toml and apply changes live (MCP servers, permissions, bash policy, hooks) |
| `/trust` | Trust this project's `.neenee/config.toml` (MCP servers + hooks) and load them |
| `/untrust` | Revoke trust for this project (disconnects MCP, unloads hooks) |
| `/skills [list\|reload]` | List or reload available skills |
| `/skill <name>` | Load a skill by name |
| `/tools` | Toggle individual session tools on or off |
| `/config` | Open user configuration |
| `/export` | Export the current conversation to the clipboard as Markdown |
| `/debug trace [on\|off]` | Toggle per-project provider round-trip tracing for debugging |
| `/debug preview` | Dry-run the next request body to a file (no provider call) |
| `/help` | Show available commands and keybindings |
| `/exit` | Exit the program |

Several interactive management commands, including `/models`, `/connections`,
`/permissions`, `/tools`, `/mcp`, `/skills`, and `/config`, are handled in the
TUI. Commands that mutate agent or session state are dispatched to the
backend.

### `/serve`

Hot-attach a WebSocket listener to the currently running session so a browser
or other client can attach (ADR-0037 §7, ADR-0054). This is a pure frontend
concern — it never reaches `SessionDriver`. See the
[Server WebSocket API](server-api.md) for the full protocol.

| Form | Effect |
|------|--------|
| `/serve [port]` | Start a loopback-only (`127.0.0.1`) listener on `port` (OS picks one if omitted). No authentication. |
| `/serve [port] --public` | Bind all interfaces (`0.0.0.0`). A bearer token is generated and printed; clients must send `Authorization: Bearer <token>` on the handshake. |
| `/serve` (no arg, while active) | Stop accepting new connections. |

The listener replays the full transcript to each new client on connect, then
streams live `AgentResponse`s. Multiple clients may attach simultaneously and
all share the same agent request queue.

## Subcommands

### `/schedule`

| Form | Effect |
|------|--------|
| `/schedule <when> <prompt>` | Schedule `<prompt>` to run at `<when>` (see below) |
| `/schedule list` | List scheduled jobs (id, kind, trigger, next fire, prompt) |
| `/schedule cancel <id>` | Cancel a scheduled job |
| `/schedule help` | Show syntax help |

`<when>` is one of:

- **a cron** — five fields `minute hour day month weekday`, recurring (e.g.
  `*/5 * * * *` every 5 min, `0 9 * * 1-5` 09:00 on weekdays);
- **a countdown** — one or more `<number><unit>` pairs from now
  (`10m`, `2h30m`, `1d12h`, `in 10 minutes`, `in 2 hours 30 minutes`;
  units: `s`/`m`/`h`/`d` and their long forms);
- **an absolute time** — `HH:MM` today (or tomorrow if already passed),
  `today HH:MM`, `tomorrow HH:MM`, `tomorrow`, `at HH:MM`,
  `YYYY-MM-DD HH:MM`, or `YYYY-MM-DDTHH:MM`.

Cron jobs **recur** (and fire their first run immediately); countdown and
absolute jobs fire **once** and are then removed. Jobs are durable (survive
restarts). Recurring cron jobs auto-expire after 30 days. `/schedule` is the
clock-driven scheduler for autopilot, reminders, and one-shot timers.

### `/repeat`

| Form | Effect |
|------|--------|
| `/repeat <cron> <prompt>` | Schedule `<prompt>` on the five-field `<cron>` and run it now (cron-only alias for `/schedule`) |
| `/repeat list` | List scheduled jobs (id, kind, trigger, next fire, prompt) |
| `/repeat cancel <id>` | Cancel a scheduled job |
| `/repeat help` | Show cron syntax help |

`/repeat` is retained as a cron-only alias for `/schedule`. Use `/schedule` for
countdown (`10m`) or absolute-time (`14:00`, `tomorrow 09:00`) one-shots.

### `/session`

| Form | Effect |
|------|--------|
| `/session status` | Show session id, parent, counts, checkpoint, compaction |
| `/session list` | List durable session branches |
| `/session resume [id]` | Resume the most recent or selected session |
| `/session fork` | Fork the current conversation into a child session |
| `/session open <id-prefix>` | Open a session by id or id prefix |
| `/session new` | Start a new durable session |

### `/permissions`

| Form | Effect |
|------|--------|
| `/permissions` | List always-allowed tool rules for this process |
| `/permissions clear` | Clear process-local always-allow rules |

### `/tools`

| Form | Effect |
|------|--------|
| `/tools` | Open the tools manager overlay |

Opens a centered, scrollable list of every tool available to the live session —
builtins and `mcp:<server>` tools — each with its source and an
`[on]`/`[off]` badge. `↑`/`↓` move the selection, `Space` toggles a tool on or
off (the harness applies it and replies with a fresh snapshot), and `Esc`
closes. `/tools` is handled entirely in the TUI and is never forwarded to the
backend.

### `/config`

| Form | Effect |
|------|--------|
| `/config` | Open the Settings overlay |

The Settings overlay exposes Appearance and Layout. Appearance offers the
`zen`, `midnight`, `nord`, `catppuccin`, and `paper` presets. The Custom option
opens an eight-field `#RRGGBB` editor for background, surface, text, muted,
accent, success, warning, and error colors. Valid custom colors preview live;
`Enter` saves and applies the palette, while `Esc` cancels the draft. Changes
apply immediately and persist in the `[tui]` table of `config.toml`.

### `/autopilot`

| Form | Effect |
|------|--------|
| `/autopilot` | Toggle autopilot on/off |
| `/autopilot on` | Run without human intervention (no confirmations, no questions) |
| `/autopilot off` | Restore interactive prompts |

When on, the agent acts without human intervention: tool permissions
auto-approve before write/execute tools (`bash`, `write_file`,
`edit_file`, …), the `ask_user` question tool is reclaimed (hidden from
the model; any stale call short-circuits), interactive command stdin is
closed instead of prompting, and the system prompt is told no human is
reachable. Affects the live process only. For the design intent and every
surface the flag enforces, see
[Autopilot operation](../explanation/agent-design/autopilot.md).

### `/principal`

| Form | Effect |
|------|--------|
| `/principal <role>` | Switch the active principal role (persona + capability scope) |
| `/principal` | List the available roles and the current one |

Switches the live agent's principal role at runtime (ADR-0053). Each role is
a preset over the product's base identity — the mission/persona shifts, the
product identity stays. It can also be triggered mid-message with the
`@principal:<role>` mention:

| Role | Scope |
|------|-------|
| `code` | The default coding principal — full capabilities, unrestricted writes |
| `architect` | Design and review focus — full read, writes retained but the persona steers toward analysis and written rationale before changes |
| `reviewer` | Read-only code review — read/search/inspect tools only (no `write_file`, `edit_file`, or `bash`) |
| `security` | Read-only, command-confined security audit — read/search plus a narrow command allowlist |

Unknown role names are rejected with the list of valid roles.

### `/btw`

| Form | Effect |
|------|--------|
| `/btw` | Open a side conversation that runs alongside the main session |

Opens a lightweight side conversation for asking quick questions without
disturbing the main session context
([ADR-0017](../adr/0017-side-conversations.md)).

### `/review`

On-demand only — triggers a bounded read-only `REVIEW` envoy that
diagnoses the current round and reports verdicts. `/review` takes no
arguments. The original periodic-cadence design
([ADR-0016](../adr/0016-session-review-over-round-counting.md)) was
superseded by on-demand review plus in-loop steering
([ADR-0030](../adr/0030-early-loop-intervention-and-round-hook.md));
ADR-0016 itself remains Accepted.

### `/search`

| Form | Effect |
|------|--------|
| `/search <query>` | Semantic search over the project's session history |

Returns the most relevant past messages for the query (see the
`search_history` tool). Useful for recalling earlier decisions.

### `/skills`

| Form | Effect |
|------|--------|
| `/skills` | List available skills (alias for `/skills list`) |
| `/skills list` | List available skills |
| `/skills reload` | Rescan local skill directories and refetch remote repositories |

### `/skill`

| Form | Effect |
|------|--------|
| `/skill <name>` | Load a skill by name into the conversation context |

### `/init`

| Form | Effect |
|------|--------|
| `/init [path]` | Initialize a `.neenee/` config tree; `path` defaults to `.` |

### `/reload`

| Form | Effect |
|------|--------|
| `/reload` | Re-read `config.toml` and apply changes live |

Re-loads configuration without restarting: MCP servers, permissions,
`bash_policy`, and hooks are all applied to the live process.

### `/trust` and `/untrust`

| Form | Effect |
|------|--------|
| `/trust` | Trust this project's `.neenee/config.toml` (MCP servers + hooks) and load them |
| `/untrust` | Revoke trust for this project (disconnects MCP, unloads hooks) |

A project's `.neenee/config.toml` is only loaded after you trust the project;
`/trust` grants that once, and `/untrust` revokes it. The trust posture is
defined in [ADR-0085](../adr/0085-config-time-tool-scoping.md).

### `/export`

| Form | Effect |
|------|--------|
| `/export` | Render the live conversation as Markdown — metadata header (session id, provider/model, exported-at), then a chronological transcript of user prompts, assistant replies, tool calls, and inlined tool results — and copy it to the system clipboard so it can be pasted into another agent to continue the work. |

The receiving agent gets the full chain of decisions and side effects: hidden
and system messages are skipped (mirroring TUI rendering), reasoning traces
are folded into collapsible `<details>` blocks, and envoy transcripts
nested under `envoy` results are summarised by message counts instead of
dumped in full. If the system clipboard is unavailable, the export falls
back to OSC52 or surfaces the underlying clipboard error.

## Custom commands

Markdown files discovered in `.neenee/commands/` (project-local, higher
priority) and `$XDG_DATA_HOME/neenee/commands/` (user-global, XDG; default
`~/.local/share/neenee/commands/`). The legacy pre-XDG fallback
`~/.neenee/commands/` was removed (ADR-0013 → ADR-0058); the filename stem or
frontmatter `name` becomes `/name` after lowercasing and stripping a leading
`/`. Names allow ASCII letters, digits, `-`, and `_`.

See [Paths](paths.md) for the full override stack and the project-vs-XDG
boundary.

Optional YAML frontmatter:

```yaml
---
name: review
description: Review changes
---
```

The template body supports `$ARGUMENTS` (the full argument string) and `$1`
through `$9` positional placeholders. Built-in command names are reserved and
are not shadowed by custom commands.

## See also

- [Harness architecture](../explanation/agent-design/harness.md) — the round
  loop, durable session, permission broker, context compaction
- [Modals](tui/modals.md) — the `/models`, `/connections`, and `/sessions` pickers
