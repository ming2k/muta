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
| `/compact` | Compact older complete turns now |
| `/clear` | Clear the conversation history |
| `/permissions [clear]` | Show or clear always-allowed tool rules |
| `/unattended [on\|off]` | Toggle unattended mode (agent runs without human intervention) |
| `/review` | Run an on-demand session-review diagnostic of the current round |
| `/search <query>` | Semantic search over the project's session history |
| `/session [status\|list\|resume\|fork\|open\|new]` | Manage durable sessions |
| `/sessions` | Browse past sessions |
| `/btw` | Open a side conversation that runs alongside the main session |
| `/resume [id]` | Resume the most recent or selected session |
| `/pursue [condition\|status\|stop\|done\|edit\|clear]` | Pursue a pursuit: the harness keeps the round going until the condition is met |
| `/repeat [cron prompt\|list\|cancel id]` | Schedule a prompt on a cron expression |
| `/init [path]` | Initialize a `.neenee/` config tree |
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
`/tools`, and `/config`, are handled in the TUI. Commands that mutate agent or
session state are dispatched to the backend.

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

### `/pursue`

| Form | Effect |
|------|--------|
| `/pursue <condition>` | Set the condition as the active pursuit, arm the stop-gate, and drive the round until it is met |
| `/pursue` | Re-arm and drive a pursuit on the existing active pursuit |
| `/pursue status` | Show the current pursuit, armed state, and gate iteration |
| `/pursue edit <condition>` | Rewrite the condition of the current pursuit |
| `/pursue done` | Mark the pursuit completed (disarms the gate) |
| `/pursue stop` | Stop the active pursuit |
| `/pursue clear` | Remove the pursuit (disarms and clears) |

`/pursue` arms a **stop-gate**: each time the model would end the round, the
harness re-injects the condition and forces another turn until the model
signals completion (`[NEENEE_PURSUIT_COMPLETE]`), the 50-turn safety cap is hit,
or the user interrupts (`/pursue stop` / `Esc`). Pursuit state is persisted per
session in SQLite, so it survives restarts and is restored on `/resume`. See
[Pursuits and the pursue stop-gate](../explanation/agent-design/pursuits.md).

### `/repeat`

| Form | Effect |
|------|--------|
| `/repeat <cron> <prompt>` | Schedule `<prompt>` on the five-field `<cron>` and run it now |
| `/repeat list` | List scheduled jobs (id, cron, next fire, prompt) |
| `/repeat cancel <id>` | Cancel a scheduled job |
| `/repeat help` | Show cron syntax help |

`<cron>` is five fields — `minute hour day-of-month month day-of-week` — e.g.
`*/5 * * * *` (every 5 minutes), `0 9 * * 1-5` (09:00 on weekdays). Jobs are
durable (survive restarts) and auto-expire after 30 days. `/repeat` is a
clock-driven scheduler, independent of `/pursue`. See
[Pursuits and the pursue stop-gate](../explanation/agent-design/pursuits.md).

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

### `/unattended`

| Form | Effect |
|------|--------|
| `/unattended` | Toggle unattended on/off |
| `/unattended on` | Run without human intervention (no confirmations, no questions) |
| `/unattended off` | Restore interactive prompts |

When on, the agent acts without human intervention: tool permissions
auto-approve before write/execute tools (`bash`, `write_file`,
`edit_file`, …), the `ask_user` question tool is reclaimed (hidden from
the model; any stale call short-circuits), interactive command stdin is
closed instead of prompting, and the system prompt is told no human is
reachable. Affects the live process only. For the design intent and every
surface the flag enforces, see
[Unattended operation](../explanation/agent-design/unattended.md).

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

### `/export`

| Form | Effect |
|------|--------|
| `/export` | Render the live conversation as Markdown — metadata header (session id, provider/model, pursuit, exported-at), pursuit checklist, then a chronological transcript of user prompts, assistant replies, tool calls, and inlined tool results — and copy it to the system clipboard so it can be pasted into another agent to continue the work. |

The receiving agent gets the full chain of decisions and side effects: hidden
and system messages are skipped (mirroring TUI rendering), reasoning traces
are folded into collapsible `<details>` blocks, and envoy transcripts
nested under `envoy` results are summarised by message counts instead of
dumped in full. If the system clipboard is unavailable, the export falls
back to OSC52 or surfaces the underlying clipboard error.

## Custom commands

Markdown files discovered in `.neenee/commands/` (project-local, higher
priority), `$XDG_DATA_HOME/neenee/commands/` (user-global, XDG; default
`~/.local/share/neenee/commands/`), and `~/.neenee/commands/` (legacy
pre-XDG fallback, emits a deprecation warning — see
[ADR-0013](../adr/0013-skills-xdg-paths-and-bundled-embed.md)). The
filename stem or frontmatter `name` becomes `/name` after lowercasing and
stripping a leading `/`. Names allow ASCII letters, digits, `-`, and `_`.

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

- [Harness architecture](../explanation/agent-design/harness.md) — pursuit state, autonomous
  loop, durable session, permission broker, context compaction
- [Modals](tui/modals.md) — the `/models`, `/connections`, and `/sessions` pickers
