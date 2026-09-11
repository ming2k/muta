# Slash commands

Built-in commands typed in the input box. The descriptions in this table are
the canonical source of truth and match the slash-suggestion popup and the
completion list exactly.

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
| `/unattended [on\|off]` | Toggle unattended execution mode (aliases: `/auto`, `/delegate`) |
| `/confinement [on\|off]` | Toggle workspace filesystem confinement for this session (aliases: `/unconfine`, `/jail`) |
| `/persona [code\|architect\|reviewer\|security\|conversational]` | Switch agent persona and capability — switches active identity and capability scope (aliases: `/role`, `/master`, `/preset`) |
| `/search <query>` | Lexical search over the current session's transcript and command ledger |
| `/sessions [id]` | Browse past sessions; with an id, open that session immediately. The retired `/resume` and `/session` are hidden aliases (legacy grammar still resolves) |
| `/fork` | Fork the current conversation into a child session |
| `/tree` | Visual DAG session tree and branch navigation |
| `/diff` | View workspace modifications made in this session |
| `/undo` | Undo the last conversation turn and file changes |
| `/dashboard` | Open the session dashboard — a full-screen live view over every daemon session (console + sessions dock), with preview / attach / interrupt / suspend / kill / prompt / create, plus the console's `@N text` addressing and `/kill` `/interrupt` `/suspend` `/new` `/help` verbs (ADR-0096; layout per ADR-0097). `Esc` leaves the screen; `Ctrl+C` follows the app-wide double-press quit. `/host` is a hidden alias |
| `/usage` | Open the usage-statistics overlay — daily token totals, per-model breakdown, and the recent request event log, aggregated over the durable store at `data/usage/` that survives session cleanup (ADR-0122) |
| `/btw [prompt\|list]` | Open a background aside conversation — asides keep running when you leave (`Ctrl+C` detaches, `Esc` interrupts, `F5` lists) |
| `/jobs [list\|kill id\|logs id]` | Inspect and manage background processes and subagents |
| `/init [path]` | Initialize a `.muta/` config tree |
| `/trust [all\|mcp\|skills\|hooks\|rules\|status\|revoke]` | Trust content-attested project asset domains; bare `/trust` means all |
| `/untrust` | Revoke all project asset-domain grants and unload their contributions |


| `/skills [list\|status]` | Browse or list available skills |
| `/tools` | Toggle individual session tools on or off |
| `/settings` | Open the Settings overlay (theme, appearance, layout, MCP). `/settings reload` re-reads config.toml and applies it live. `/config` is a hidden alias |
| `/retry` | Retry the last failed request |
| `/export` | Export the current conversation to the clipboard as Markdown |
| `/debug trace [on\|off]` | Toggle per-project provider round-trip tracing for debugging |
| `/debug preview` | Dry-run the next request body to a file (no provider call) |
| `/exit` | Exit the program (mirrors the app-wide double `Ctrl+C` quit gesture) |

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
(`TRIGGER_WORD_SUGGESTIONS` in `muta-runtime/src/startup.rs`); adding a
row extends the steering without growing the executable command surface.

### `/serve`

> **Superseded by the unified daemon (ADR-0096).** The `muta start --fg`
> daemon now owns every session and serves them all
> over the control plane; hot-attaching a listener to a single running TUI
> session is a legacy of the per-session-server model. Use `muta start` to run
> the daemon and `mutx attach` / `/dashboard` to drive its sessions. See the
> [Server WebSocket API](server-api.md) for the current protocol.

## Subcommands



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
identically but is not advertised in completion, so new users are
steered to the canonical spelling.

The Settings overlay exposes Appearance and Layout. Appearance offers the
`zen`, `midnight`, `nord`, `catppuccin`, and `paper` presets. The Custom option
opens an eight-field `#RRGGBB` editor for background, surface, text, muted,
accent, success, warning, and error colors. Valid custom colors preview live;
`Enter` saves and applies the palette, while `Esc` cancels the draft. Changes
apply immediately and persist in the `[tui]` table of `config.toml`.

### `/unattended`

| Form | Effect |
|------|--------|
| `/unattended` | Toggle unattended autonomous mode on/off (aliases: `/auto`, `/delegate`) |
| `/unattended on` | Empower AI to run unattended and auto-approve tool permissions without prompts |
| `/unattended off` | Restore interactive confirmation and question prompts |

When on, the agent runs unattended: tool executions and file modifications are automatically approved without prompting, and ambiguity questions (`ask_user`) are resolved self-reliantly by the model. Dangerous command hard denies (such as root-level destructive commands) remain blocked. The posture is persisted on the session: a daemon crash, kill, upgrade, or reboot reopens the session in the same posture.

### `/confinement`

| Form | Effect |
|------|--------|
| `/confinement` | Toggle workspace filesystem confinement on/off (aliases: `/unconfine`, `/jail`) |
| `/confinement on` | Enable workspace confinement (confine file tools to workspace root and temp) |
| `/confinement off` | Disable workspace confinement (allow full host filesystem access) |

When confinement is disabled (`off`), file tools can read and write any path on the host system bounded only by the OS user permissions of the daemon. When enabled (`on`, default), file operations outside admitted workspace roots are blocked.

### `/persona`

| Form | Effect |
|------|--------|
| `/persona <id>` | Switch the active agent persona (identity, capability scope, and session metadata) (aliases: `/role`, `/master`, `/preset`) |
| `/persona` | List available personas (shipped presets and user-configured personas) and the current one |

Switches the session's agent persona and capability at runtime (ADR-0053, updated by ADR-0183 and ADR-0225). Switching resolves against user-configured personas in `~/.config/muta/personas.toml` first, then falls back to built-in presets:

| Built-in Preset | Scope |
|-----------------|-------|
| `code` | The default developer master — full native capabilities, unrestricted writes, no role directive |
| `architect` | Design and review focus — full read, writes retained but directive steers toward analysis and design rationale |
| `reviewer` | Read-only code review — read/search/inspect tools only (no `write_file`, `edit_text`, or `execute_command`) |
| `security` | Read-only, command-confined security audit — read/search plus narrow command allowlist |
| `code_analyst` | Read-only code analysis & sandboxed execution |
| `conversational` | Workspace-free conversational companion — web + ask_user tools only |

Unknown persona identifiers are rejected with the list of valid personas. Legacy `/role`, `/master`, and `/preset` calls are transparently accepted as aliases.

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
| `/skills` | Browse skills modal in TUI, or list all skills (with scope and trust status) in CLI |
| `/skills list` | List available skills with scope, version, and status |
| `/skills status` | Show aggregate health, trust state, and source distribution |

### `/init`

| Form | Effect |
|------|--------|
| `/init [path]` | Initialize a `.muta/` config tree; `path` defaults to `.` |

### `/trust` and `/untrust`

| Form | Effect |
|------|--------|
| `/trust` or `/trust all` | Trust and load every present project asset domain (MCP, skills, hooks, rules, roots) |
| `/trust mcp` | Trust only project MCP definitions |
| `/trust skills` | Trust only project skills |
| `/trust hooks` | Trust only project lifecycle hooks |
| `/trust rules` | Trust only project rules and instructions |
| `/trust roots` | Trust project-declared linked workspace roots (`[workspace].additional_roots`) |
| `/trust status` | Show `mcp`, `skills`, `hooks`, `rules`, and `roots` states plus a display-only aggregate |
| `/trust revoke` or `/untrust` | Revoke all domain grants; disconnect project MCP and unload project hooks, skills, rules, and linked workspace roots |

The five domains are attested independently. MCP covers `.muta/mcp.json` and
the `[mcp]` projection of `.muta/config.toml`; Skills covers `.muta/skills`,
`.agents/skills`, `.claude/skills`, and `skills`; Hooks covers `.muta/hooks`
and project `[[hooks]]`; Rules covers project instructions and
`.muta/commands`; Roots covers project `[workspace].additional_roots`.
Changing one domain moves only that domain to `changed`.

Trust is keyed to the canonical workspace root. Each domain digest includes
paths, file bytes, and relevant permission modes; symlinks and unreadable or
unsupported entries fail closed. A trust or revoke command applies live to
every consumer. Project skills, MCP/hooks, and linked roots re-attest before use so changed
content cannot execute or widen boundaries under a stale grant.

Asset trust gates project-authored contributions. Untrusted project roots
remain quarantined and do not widen the filesystem boundary until explicitly
trusted via `/trust` or `/trust roots`. `/trust workspace`, `/trust extensions`,
and `/extensions` have been removed rather than retained as ambiguous aliases.

### `/export`

| Form | Effect |
|------|--------|
| `/export` | Render the live conversation as Markdown — metadata header (session id, provider/model, exported-at), then a chronological transcript of user prompts, assistant replies, tool calls, and inlined tool results — and copy it to the system clipboard so it can be pasted into another agent to continue the work. |

The receiving agent gets the full chain of decisions and side effects: hidden
and system messages are skipped (mirroring TUI rendering), reasoning traces
are folded into collapsible `<details>` blocks, and subagent transcripts
nested under `subagent` results are summarised by message counts instead of
dumped in full. If the system clipboard is unavailable, the export falls

back to OSC52 or surfaces the underlying clipboard error.

## Custom commands

Markdown files discovered in `.muta/commands/` (project-local, higher
priority) and `$XDG_DATA_HOME/muta/commands/` (user-global, XDG; default
`~/.local/share/muta/commands/`). The legacy pre-XDG fallback
`~/.muta/commands/` was removed (ADR-0013 → ADR-0058); the filename stem or
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
