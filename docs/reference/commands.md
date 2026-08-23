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
| `/new` | Start a new session, keeping the current one in history. Typing the retired `/clear` (or `/reset`) suggests `/new` instead — it never wipes anything in place |
| `/permissions [clear]` | Show or clear always-allowed tool rules |
| `/autopilot [on\|off]` | Toggle autopilot mode (agent runs without human intervention) |
| `/principal <code\|architect\|reviewer\|security>` | Switch the principal role — changes persona and capability scope |
| `/search <query>` | Lexical search over the current session's transcript and command ledger |
| `/sessions [id]` | Browse past sessions; with an id, open that session immediately. The retired `/resume` and `/session` are hidden aliases (legacy grammar still resolves) |
| `/fork` | Fork the current conversation into a child session |
| `/dashboard` | Open the session dashboard — a full-screen live view over every daemon session (console + sessions dock), with preview / attach / interrupt / suspend / kill / prompt / create, plus the console's `@N text` addressing and `/kill` `/interrupt` `/suspend` `/new` `/help` verbs (ADR-0096; layout per ADR-0097). `Esc` leaves the screen; `Ctrl+C` follows the app-wide double-press quit. `/host` is a hidden alias |
| `/usage` | Open the usage-statistics overlay — daily token totals, per-model breakdown, and the recent request event log, aggregated over the durable store at `data/usage/` that survives session cleanup (ADR-0122) |
| `/btw [prompt\|list]` | Open a background aside conversation — asides keep running when you leave (`Ctrl+C` detaches, `Esc` interrupts, `F5` lists) |
| `/repeat [cron prompt\|list\|cancel id]` | Schedule a prompt on a cron expression (cron-only alias for `/schedule`) |
| `/schedule [when prompt\|list\|cancel id]` | Schedule a prompt: cron (recurring) or countdown/absolute-time (one-shot) |
| `/init [path]` | Initialize a `.neenee/` config tree |
| `/trust` | Trust this project's `.neenee/` contributions (MCP servers, hooks, project skills, project commands) and load them |
| `/untrust` | Revoke trust for this project (disconnects MCP, unloads hooks, skills, and commands) |
| `/skills [list\|reload]` | List or reload available skills |
| `/skill <name>` | Load a skill by name |
| `/tools` | Toggle individual session tools on or off |
| `/settings` | Open the Settings overlay (theme, appearance, layout, MCP). `/settings reload` re-reads config.toml and applies it live. `/config` is a hidden alias |
| `/retry` | Retry the last failed request |
| `/export` | Export the current conversation to the clipboard as Markdown |
| `/debug trace [on\|off]` | Toggle per-project provider round-trip tracing for debugging |
| `/debug preview` | Dry-run the next request body to a file (no provider call) |
| `/help` | Show available commands and keybindings |
| `/exit` | Exit the program |

Several interactive management commands, including `/models`, `/connections`,
`/permissions`, `/tools`, `/mcp`, `/skills`, and `/settings`, are handled in the
TUI. Commands that mutate agent or session state are dispatched to the
backend.

### Trigger-word suggestions ("did you mean …")

Retired commands and common synonyms are not executable — there is no
`/clear`, `/reset`, or `/continue`. Typing one pins a suggestion row on top of
the completion popup pointing at the supported command, and accepting the row
rewrites the input to it:

| You type | Suggested | Why |
|----------|-----------|-----|
| `/clear` | `/new` | Clearing the transcript in place was removed; a fresh session keeps the old one on disk |
| `/reset` | `/new` | Same fresh-session semantics |
| `/continue` | `/sessions` | Picks a session up where it left off |
| `preferences` | `/settings` | Settings holds every user preference |
| `options` | `/settings` | Same surface, natural spelling |
| `theme` | `/settings` | Appearance lives in the Settings overlay |
| `themes` | `/settings` | Same |
| `appearance` | `/settings` | Same |

The mapping is presentation-only and lives in one table
(`TRIGGER_WORD_SUGGESTIONS` in `neenee-runtime/src/startup.rs`); adding a
row extends the steering without growing the executable command surface.

### `/serve`

> **Superseded by the unified daemon (ADR-0096).** The `neenee daemon start --fg`
> daemon now owns every session and serves them all
> over the control plane; hot-attaching a listener to a single running TUI
> session is a legacy of the per-session-server model. Use `neenee daemon start` to run
> the daemon and `neenee attach` / `/dashboard` to drive its sessions. See the
> [Server WebSocket API](server-api.md) for the current protocol.

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

### `/sessions`

| Form | Effect |
|------|--------|
| `/sessions` | Open the sessions picker (Enter opens, `i` info, `d` delete, `n` new) |
| `/sessions <id-prefix>` | Open that session immediately |

`/resume` and `/session` are retired hidden aliases of `/sessions`; legacy
grammar still resolves (`/resume <id>` and `/session open <id>` open the
session, `/session list` opens the picker, `/session new` and
`/session fork` behave like `/new` and `/fork`). `/session status` is gone —
session id, counts, and timestamps live in the picker's `i` info view.

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

### `/settings`

| Form | Effect |
|------|--------|
| `/settings` | Open the Settings overlay |
| `/settings reload` | Re-read `config.toml` and apply changes live |

`/config` is a hidden alias for `/settings`: it parses and dispatches
identically but is not advertised in completion or `/help`, so new users are
steered to the canonical spelling.

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
reachable. The posture is persisted on the session
([ADR-0132](../adr/0132-session-persisted-autopilot-posture.md)): a daemon
crash, kill, upgrade, or reboot reopens the session in the same posture, so
an accidentally-interrupted unattended run picks up where it left off
(pair with `/retry` to resume the stopped round). `/reset` starts the new
session attended. For the design intent and every
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
| `/btw` | Open a **new** aside view — nothing is sent yet |
| `/btw <text>` | Open a new aside and immediately send `<text>` as its first turn |
| `/btw list` | Open the asides list modal (same as `F5`) |

Opens a turn-level aside conversation forked from the current context
(keeping the complete prior context — especially the previous full turn)
that runs alongside the main session. Leaving the aside view (`Ctrl+C`)
**detaches without interrupting**: the aside keeps running in the
background, stays in the asides list (`F5`), and can be re-entered with its
full transcript at any time. `Esc` interrupts the viewed aside's round
without closing it. An aside opened but never used is discarded on detach —
it never appears in the list or `/sessions`
([ADR-0103](../adr/0103-btw-background-asides.md), which extends
[ADR-0017](../adr/0017-side-conversations.md)).

### `/search`

| Form | Effect |
|------|--------|
| `/search <query>` | Lexical search over the current session's transcript and command ledger |

Ranks the session's messages and command results against the query terms
(deterministic lexical scoring — no index file, nothing persisted). Useful for
recalling earlier decisions inside one long session; cross-session recall is
`/sessions` + `/export`.

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

### `/trust` and `/untrust`

| Form | Effect |
|------|--------|
| `/trust` | Trust this project's `.neenee/` contributions (MCP servers, hooks, project skills, project slash commands) and load them |
| `/untrust` | Revoke trust for this project (disconnects MCP, unloads hooks, skills, and commands) |

Project trust (ADR-0085 §5, extended by ADR-0107) gates every
project-supplied capability. While a project is untrusted, its
`.neenee/config.toml` `[mcp.*]` servers and `[[hooks]]`, its project skills
(`.neenee/skills`, `.agents/skills`, `.claude/skills`), and its project
slash commands (`.neenee/commands/`) are not loaded at all — the trust store
is consulted at scan time, so every path (startup, the periodic skills
refresh, `/skills reload`) is gated identically. `/trust` applies
immediately: project skills rescan in, and project slash commands become
runnable through a trust-checked dispatcher fallback. When a trusted
project's skill or command reuses the name of a user-scope entry, the
project entry wins by priority and the harness emits a one-time warning
notice naming the winner, so silent overrides are impossible. Untrusted
projects get a startup notice listing exactly what stayed disabled until
`/trust`. The trust root is git-aware: one grant covers the repo's
subdirectories and linked worktrees.

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
