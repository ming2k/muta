# Configuration Reference

Every option in `config.toml`, with its default. The file lives at
`$XDG_CONFIG_HOME/muta/config.toml` — see [Paths](paths.md) for the resolved
location and override precedence.

All keys are optional: a missing key, a missing table, or an absent file uses
the defaults below. Unknown keys are ignored, so removing or renaming a key
never breaks parsing.

## Compaction

Context compaction keeps the uncapped agentic loop bounded. Thresholds are
derived from the **active model's context window** (token-denominated) and
re-seeded on every provider switch, so they track the live model rather than a
fixed budget. See the [harness explanation](../explanation/agent-design/harness.md#context-projection),
the [pruning](../explanation/agent-design/context-pruning.md) and
[compaction](../explanation/agent-design/context-compaction.md) deep-dives, and
ADR-0019 / ADR-0021 for the design.

Pressure estimates the complete next request: prepared conversation messages,
the regenerated system prompt, newly injected skills, and visible tool schemas.
Each fraction is multiplied by the active model's full context window (`0` →
the fallback window) to produce an absolute threshold. The gap above
`compaction.utilization` reserves room for protocol framing and the next model
completion.

The removed `compaction.max_active_tokens` and
`compaction.prompt_reserve_tokens` keys are ignored when loading older config
files. Performance limits are not part of context-capacity safety policy.

| Key | Default | Meaning |
|-----|---------|---------|
| `compaction.utilization` | `0.85` | Trigger a full summarizing compaction once pressure reaches this fraction of the window |
| `compaction.target_utilization` | `0.25` | After a full compaction, compress the model window down to this fraction |
| `compaction.prune_utilization` | `0.65` | Trigger cheap tool-result pruning at this fraction (below `utilization`) |
| `compaction.fallback_window_tokens` | `32000` | Assumed window (tokens) when the model's context window is unknown |
| `compaction.preserve_rounds` | `6` | Number of recent complete user rounds kept verbatim after a full compaction. The former key `compaction_preserve_turns` is **not** aliased (ADR-0120): it parses as an unknown key, is ignored, and is dropped on the next save — run `muta config check` to find stale spellings |
| `compaction.summarize` | `true` | Use the active model for an anchored structured summary; `false` uses the deterministic excerpt fallback |
| `compaction.prune` | `true` | Enable cheap tool-result pruning (pre-round and mid-round) |
| `compaction.prune_protect_tokens` | `6000` | Most recent tool results (tokens) protected from pruning |

Resolved thresholds per model (defaults):

| Model | Window | Prune at | Compact at | Target |
|-------|--------|----------|------------|--------|
| `glm-5.2`, Gemini, DeepSeek | 1,000,000 | 650,000 | 850,000 | 250,000 |
| `k3` | 1,048,576 | 681,574 | 891,289 | 262,144 |
| `kimi-k2.7-code` | 262,144 | 170,393 | 222,822 | 65,536 |
| `gpt-4o` | 128,000 | 83,200 | 108,800 | 32,000 |
| unknown / local | 32,000 (fallback) | 20,800 | 27,200 | 8,000 |

```toml
[compaction]
utilization = 0.85
target_utilization = 0.25
prune_utilization = 0.65
fallback_window_tokens = 32000
preserve_rounds = 6
summarize = true
prune = true
prune_protect_tokens = 6000
```

## Master agent behavior

The optional `[master]` table.

| Key | Default | Meaning |
|-----|---------|---------|
| `master.hard_stop_turns` | `0` | Hard-stop a round after this many ReAct turns. `0` = uncapped (the only execution cap; compaction is the backstop) |
| `master.allow_model_stdin` | `false` | Whether the model may supply `stdin` bytes for an `execute_command` call it emits. Off by default: the execute_command schema exposes no `stdin` parameter and a command needing input either gets it from a human (interactive classifier → inline input panel) or fails fast with a non-interactive remedy hint (see ADR-0043). On: the execute_command schema dynamically adds a `stdin` field the model can fill, threaded through as a prefilled pipe — for delegated automatic flows where no human is reachable |
| `master.skip_interactive_input` | `false` | Whether an interactive `execute_command` invocation (matched by the interactive classifier: `sudo`/`gpg`/`passwd`/TUI editors/`read`/…) **never** pops the inline input panel. Off by default: a command needing input prompts you with an input panel (command + masked/plain field). On: the panel is skipped and the command runs with stdin closed — it reads EOF immediately and fails fast with a non-interactive remedy hint, exactly as in delegated autonomous mode. For users who find the prompt disruptive and would rather retry the command themselves. Note: this only governs the interactive-input path; it does not put the master into delegated autonomous mode, so ordinary tool confirmations still apply |
| `master.doom_guard.enabled` | `true` | Doom-loop guard: blocks a watched tool signature once it recurs enough times to reach `threshold` within the window. On by default — a model making progress never trips it, and the cheapest token-burning loop (`sleep N; make` variants) is capped at its third occurrence (ADR-0148). Forced off for subordinate runners |
| `master.doom_guard.window` | `16` | Number of recent watched tool signatures retained for repeat detection |
| `master.doom_guard.threshold` | `3` | Occurrences in-window before a repeat is blocked. `3` (ADR-0148): one same-signature re-run is tolerated — a transient retry, a re-run of the same test after an edit — and the second repeat is blocked. `2` restores the strict ADR-0113 first-repeat block. Clamped to `>= 2` |

```toml
[master]
hard_stop_turns = 0
allow_model_stdin = false
skip_interactive_input = false

[master.doom_guard]
enabled = true
window = 16
threshold = 3
```


## Connection selection and retry

| Key | Default | Meaning |
|-----|---------|---------|
| `default_connection` | `""` (empty) | Connection id for the **fresh-session default**: used at startup and updated by a `/models` switch so the next launch follows it; empty leaves the choice to the `/models` picker. A switch also pins the selection to the session so resume restores it |
| `default_model` | `""` (empty) | Active model id within the selected connection, written by a `/models` switch or add-connection flow alongside `default_connection`. |
| `connection_retry_max_attempts` | `30` | Max retry attempts for a transient connection error within a turn (clamped to 1..60) |
| `connection_retry_base_ms` | `1000` | Base delay for exponential backoff, in milliseconds |
| `connection_retry_max_ms` | `10000` | Cap on the backoff delay, in milliseconds |

## Connection instances and credentials

Connection *instances* (the "who I connect to" records) live in the state store
`$XDG_STATE_HOME/muta/connections.toml`; secrets in
`$XDG_CONFIG_HOME/muta/credentials.toml`; `config.toml` holds only the
*selection* (`default_connection` / `default_model`, which reference instance
ids). The routes a model actually travels (per-model protocol/dialect/endpoint/
reasoning) are **derived at runtime** from each instance's preset and the
discovery cache — never persisted, so two instances of the same preset can
never duplicate or drift a route set. See [Providers](providers.md) for the
matrix, [Paths](paths.md) for the files, and [Add a provider](../how-to/add-a-provider.md)
for the full workflow.

An instance is declared as one `[[connections]]` table in `connections.toml`:

```toml
[[connections]]
id = "acme"
name = "Acme Relay"          # display name; defaults to the id
auth = "ApiKey"              # ApiKey | XaiOAuth | ChatGptOAuth | CopilotOAuth | AntigravityOAuth
# api_key_env = "ACME_API_KEY"  # optional env var holding the credential

# Pure-custom instance only (no preset_id):
protocol = "openai-chat-completions"
# Also valid: openai-responses | anthropic-messages | google-generate-content
base_url = "https://relay.example.com/v1/chat/completions"
models = ["acme-7b", "acme-13b"]
```

The credential for an instance is stored once in `credentials.toml`, keyed by connection id:

```toml
[connections]
acme = "sk-..."
```

Resolution precedence is **`api_key_env` env var > `credentials.toml`** — an
instance declares an optional env var *name*; when set and populated it wins.

Multiple instances of the same preset are ordinary: each is its own
`[[connections]]` row referencing the same `preset_id`, differing only in
identity, credential, and overrides. The preset defines the routes once;
instances never repeat them.

| `favorites` | Default | Meaning |
|-----|---------|---------|
| `favorites` | `[]` | Favorite **model ids** pinned for quick access in the picker (ADR-0046 made favorites per-model). Flat list of model wire ids; a starred daily-driver model sorts into the second priority tier (below the currently-active pair) wherever it is served |

## Permissions, command policy, and tool variants

The optional `[permissions]` and `[bash_policy]` tables govern the permission
broker and the command guard (`[bash_policy]`, the config key retained from the tool's original name).

| Key | Default | Meaning |
|-----|---------|---------|
| `permissions.allow` | `[]` | Rules to pre-seed the "always allow" allowlist at startup: each rule is a `{ tool, scope }` pair. `scope = "*"` matches every call to the tool; any other value must match the call's scope exactly (a full path, or the exact command string for `execute_command`) — no prefix or substring matching |
| `bash_policy.enabled` | `true` | Master switch for the bash policy guard; dangerous built-in commands stay protected even when `execute_command` is broadly allowed |
| `bash_policy.allow_user_override_builtin_deny` | `false` | Whether an explicit user `allow` rule may override a compiled-in `deny` rule (user `allow` can still override compiled-in `confirm` rules) |
| `bash_policy.rules` | `[]` | User rules evaluated before built-in `confirm` rules: each rule is a `{ name, match, pattern, action, reason }` tuple. `match` is `"regex"` (default), `"contains"`, `"startswith"`, or `"program"`; `action` is `"allow"`, `"confirm"`, or `"deny"` |

```toml
[permissions]
allow = [
  { tool = "execute_command", scope = "git status" },
  { tool = "hook", scope = ".muta/hooks/lint.sh" },
]

[bash_policy]
enabled = true
allow_user_override_builtin_deny = false

[[bash_policy.rules]]
name = "block rm -rf"
match = "regex"
pattern = "^rm -rf"
action = "deny"
reason = "Destructive command"
```

The optional `[tool_variants]` table pins per-model tool variant selections
(`capability → variant_id`), one table per model id — e.g. which tool schema
variant a model receives for a capability with several implementations.

## Additional workspace roots

The optional `[workspace]` table widens native file tools' admitted boundary for cross-project work.
It can be configured in two places:
1. **Global Configuration** (`$XDG_CONFIG_HOME/muta/config.toml`): User-owned baseline, active across all projects.
2. **Project Configuration** (`.muta/config.toml`): Declared by the project repository and active when the workspace's `roots` domain is trusted via `/trust` or `/trust roots`. Untrusted project declarations remain quarantined and do not widen boundaries until reviewed and approved.

| Key | Default | Meaning |
|-----|---------|---------|
| `workspace.additional_roots` | `[]` | Extra directories admitted alongside the project root. Relative paths resolve against the **project root** (never the process cwd); `~` expands to the user's home. The platform temp dirs (`$TMPDIR`, `/tmp`) are always admitted implicitly and need no entry |

```toml
# .muta/config.toml (Project-local, trust-gated) or $XDG_CONFIG_HOME/muta/config.toml (Global)
[workspace]
additional_roots = ["../backend", "~/projects/design-kit"]
```

Native read, edit, write, directory, and metadata operations may address any
admitted root. Relative file paths, the shell's cwd, and project-bound asset
discovery stay anchored to the primary root. Shell commands are a separate
runtime-permission concern; linked roots neither authorize a command nor
attempt to contain arbitrary host-shell path access.

Entries are validated at session start. An entry that does not exist, is not
a directory, is the workspace root itself, or nests inside the workspace is
rejected with an error naming the entry; the session then falls back to
strict single-root containment (the reason is logged) rather than starting
half-admitted. Duplicate entries that resolve to the same directory collapse
silently.

## Per-model reasoning settings

Reasoning controls are **per route** — one (instance, model) pair — not per
provider. `effort` is the reasoning-depth throttle; `thinking` is an
Anthropic-only on/off switch. See [Reasoning effort](effort.md) for the full
per-provider mapping and how a model's effective ladder resolves; this section
covers storage only.

`effort` applies to any reasoning model whose protocol exposes a depth field —
OpenAI (Responses and chat), Anthropic, xAI Grok, Kimi K3, DeepSeek, GLM-5.2,
and Gemini. Valid values are clamped to the model's supported levels at
request-build time (`none`, `minimal`, `low`, `medium`, `high`, `xhigh`, `max`;
GPT models expose a subset). Settings are stored per `(instance, model)` in
`$XDG_STATE_HOME/muta/route_settings.json`, written by the model `e` editor in
the picker — they are user-set route facts, not disposable discovery data or
`config.toml` behavior.

Anthropic extended thinking is **opt-in** (ADR-0046). A model does not reason
unless you have configured it to.

**Opt-in rule:** a route's *presence* in `route_settings` opts the model in to
thinking. Thinking defaults **on** (the recommended Claude mode) unless the
entry explicitly sets `thinking = false`; a set `effort` applies at that depth
(else the model's default). A route **not** listed sends no `thinking` object —
it never reasons on its own. Both fields are optional within an entry.

In the TUI, drilling into a provider and pressing `e` on a model with reasoning
controls opens the per-model settings popup. OpenAI models show the Effort row;
Anthropic models show Effort plus the Thinking switch.

The legacy `[model_reasoning."<model-id>"]` table and the flat
`anthropic_effort` / `anthropic_thinking` fields are **deprecated** and no
longer read; a one-shot migration folds their values into `route_settings` for
the instances that serve the model.

### Per-route prompt caching

The same route-settings record may contain `prompt_cache`:

```json
{
  "connections": {
    "openai-work": {
      "gpt-5.6-sol": {
        "prompt_cache": {
          "mode": "explicit",
          "retention": "thirty_minutes"
        }
      }
    }
  },
  "migrated_from_cache": true
}
```

`mode` is `provider_default`, `implicit`, `automatic`, `explicit`, or
`disabled`. `retention`, when present, is `in_memory`, `five_minutes`,
`thirty_minutes`, `one_hour`, or `twenty_four_hours`.

These values are requests, not feature flags. The selected provider route and
model must explicitly advertise the mode and retention; unsupported values
fail before network dispatch. Omitting `prompt_cache` uses the route's declared
provider default. Unknown fields are rejected rather than ignored. See
[Providers](providers.md#prompt-cache-capability-matrix).

## TUI presentation

The optional `[tui]` table. Appearance and layout values can also be changed
interactively with `/settings` (alias `/config`).

## Mutx Terminal Configuration (`mutx/config.toml`)

Following ADR-0136, terminal UI presentation and input history preferences are cleanly decoupled from the core daemon into `$XDG_CONFIG_HOME/mutx/config.toml` (and prompt history in `$XDG_STATE_HOME/mutx/history.json`).

| Key | Default | Meaning |
|-----|---------|---------|
| `transcript_layout` | `"turn_band"` | Transcript grouping: `"turn_band"` (grouped ReAct turn bands) |
| `color_scheme` | `"zen"` | Active palette: `zen`, `midnight`, `nord`, `catppuccin`, `paper`, or `custom` |
| `click_outside_dismiss` | `true` | Click outside a modal to close it (mirrors Esc). On by default; the dismissable set excludes modals holding in-progress input, and the startup picker's click-outside still quits. Set `false` to require Esc / Ctrl+C for every close. |
| `expand_auto_scroll` | `false` | Whether expanding/collapsing a disclosure (tool step, command result, thinking, provider retry, notice) auto-scrolls to keep the toggled card well-placed — on expand the header shifts toward the viewport top; on collapse a scrolled-past summary is kept visible. Off by default: a toggle is a read interaction and leaves your scroll position untouched. |
| `default_expanded.<step>` | presenter default | Default expand state for a tool name or `thinking` |
| `custom_color_scheme.background` | `"#070808"` | Terminal canvas |
| `custom_color_scheme.surface` | `"#0e0f0f"` | Panels and menus |
| `custom_color_scheme.text` | `"#d5d5cd"` | Primary foreground |
| `custom_color_scheme.muted` | `"#777d75"` | Secondary foreground |
| `custom_color_scheme.accent` | `"#8ea191"` | Focus and brand color |
| `custom_color_scheme.success` | `"#759475"` | Positive states |
| `custom_color_scheme.warning` | `"#b5955d"` | Caution states |
| `custom_color_scheme.error` | `"#be6f68"` | Failure states |
| `input_history.dedup` | `true` | Collapse identical prompt text into a single entry, keyed on the text **alone** (across sessions and workspaces). Re-sending the same prompt bumps it to the top of the newest-first picker. Set `false` to keep `(text, session)` entries distinct — the same words typed in two sessions then stay as two rows, each with its own origin |
| `input_history.record_commands` | `false` | Record `/slash` command invocations (`/model`, `/new`, …) into the input history. With the default `false`, new commands are not recorded and any legacy ones stop showing in the picker. Commands are UI gestures, not prompts — they are already visible in the transcript. Set `true` to make them recallable from `Ctrl+R` again |

The custom palette contains eight `#RRGGBB` semantic colors. The renderer derives its remaining surfaces, hover states, code colors, and diff colors from these values. Custom themes can also be placed as `*.toml` files in `$XDG_CONFIG_HOME/mutx/themes/`.

```toml
transcript_layout = "turn_band"
color_scheme = "custom"
click_outside_dismiss = true
expand_auto_scroll = false

[default_expanded]
edit_text = true
execute_command = true
thinking = false

[custom_color_scheme]
background = "#070808"
surface = "#0e0f0f"
text = "#d5d5cd"
muted = "#777d75"
accent = "#8ea191"
success = "#759475"
warning = "#b5955d"
error = "#be6f68"

[input_history]
dedup = true
record_commands = false
```

## Hooks

Lifecycle event hooks (ADR-0025): each entry runs a shell command at one
point in the agent's lifecycle. See the [hooks explanation](../explanation/agent-design/hooks.md)
for the event set, the command contract, and how a `Stop` hook composes with
round termination.

The `[[hooks]]` array contains one table per hook. The capability a hook has
(block / inject / observe) is implied by its `event` — see the explanation for
which event honours which.

| Key | Default | Meaning |
|-----|---------|---------|
| `hooks[].event` | — | The lifecycle event: `SessionStart`, `SessionEnd`, `UserPromptSubmit`, `PreToolUse`, `PostToolUse`, `PostToolUseFailure`, `Stop`, `PreCompact`, `PostCompact`, `Turn` (ADR-0030, turn end), `TurnStart` (turn start), `PermissionRequest` (agent blocked on an approval prompt), `UserQuestion` (agent blocked on an `ask_user` question). `Turn`/`TurnStart` are `Deny`-forbidden (inject or observe only); `PermissionRequest`/`UserQuestion` are observe-only (fire-and-forget, ideal for desktop notifications). `PermissionRequest` honours a tool-name matcher. `RoundStart` is accepted only as the legacy alias for `TurnStart` |
| `hooks[].matcher` | `*` | Tool-name filter. A `|`-separated list of exact names (`Write|Edit`) when only letters/digits/`_`/`|`; otherwise a regular expression. Only the tool events honour it |
| `hooks[].command` | — | Shell command run when the event matches. Receives the event JSON on stdin; replies via exit code / stdout JSON. Its exact command must already be allowed as `tool = "hook"`; hooks fail closed instead of recursively opening a permission prompt from inside agent control flow |

The flat JSON for `Turn` and `TurnStart` includes `round` (one-based),
`turn` (zero-based within that round), and `consecutive_readonly`. A provider
retry remains the same turn.

```toml
[[hooks]]
event   = "PostToolUse"
matcher = "Write|Edit"
command = ".muta/hooks/lint.sh"

[[hooks]]
event   = "PreToolUse"
matcher = "Bash"
command = ".muta/hooks/guard-rm.sh"

[[hooks]]
event   = "Stop"
command = ".muta/hooks/ci-gate.sh"

# ADR-0030: fires once per ReAct turn, at turn end. Deny is ignored (no
# de-facto turn cap); inject context or observe. Carries the read-only-turn
# streak.
[[hooks]]
event   = "Turn"
command = ".muta/hooks/turn-watch.sh"

# Symmetric partner: fires at the start of each ReAct turn, after tools are
# prepared but before the next model completion. Use it to (re)inject context at
# the top of the model's attention for the turn — e.g. to re-anchor the
# principal's role after a run of read-only delegations. Deny is ignored here
# too.
[[hooks]]
event   = "TurnStart"
command = ".muta/hooks/turn-open.sh"

# Interrupt notifications (observe-only): fire-and-forget when the agent blocks
# waiting for you. The canonical use is a desktop/bell notification so a
# long-running task that runs unattended still gets your attention. Outcomes are
# ignored — these never grant/deny or alter the transcript. The matcher targets
# the tool seeking approval (here: only execute_command). `UserQuestion` has no matcher.
[[hooks]]
event   = "PermissionRequest"
matcher = "execute_command"
command = ".muta/hooks/notify.sh \"Needs approval\""
[[hooks]]
event   = "UserQuestion"
command = ".muta/hooks/notify.sh \"AI asked a question\""
```

## Feature tables

These sub-tables have their own reference pages; only the table name is
configured here.

| Table | Configures | Reference |
|-------|------------|-----------|
| `[skills]` | Skill sources, extra paths, disabled skills | [Skills](tools/skills.md) |
| `[websearch]` | Web-search backend, proxy, timeout (API keys live in `credentials.toml [websearch]`, not here) | [Web tool](tools/web.md) |
| `[mcp.<server>]` | MCP servers (one table per server) | [MCP](tools/mcp.md) |

## Daemon

The `[daemon]` table configures the user-level session daemon that owns every
session across every project (ADR-0096). It is read at daemon startup.

| Key | Default | Meaning |
|-----|---------|---------|
| `daemon.shutdown_grace_secs` | `10` | How long a `muta daemon stop` waits for hosted sessions to settle before forcing shutdown |
| `daemon.idle_exit_minutes` | `5` | A daemon with no hosted sessions exits after this idle period (armed `/schedule` jobs keep it alive — ADR-0125) |
| `daemon.local_auth` | `true` | Require the bearer token on the Unix-socket control plane. Turn off only for locked-down single-user sockets |
| `daemon.rehost_armed_schedules` | `true` | At daemon boot, rehost every persisted session that still has armed `/schedule` jobs, so scheduled prompts keep firing across daemon restarts (ADR-0125); `false` = cold start |

See [CLI reference](cli.md) for the `daemon` verbs and
[server API](server-api.md) for the wire protocol.
