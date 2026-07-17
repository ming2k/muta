# Configuration Reference

Every option in `config.toml`, with its default. The file lives at
`$XDG_CONFIG_HOME/neenee/config.toml` — see [Paths](paths.md) for the resolved
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
| `compaction_preserve_turns` | `6` | Number of recent complete user turns kept verbatim after a full compaction |
| `compaction_summarize` | `true` | Use the active model for an anchored structured summary; `false` uses the deterministic excerpt fallback |
| `compaction_prune` | `true` | Enable cheap tool-result pruning (pre-round and mid-round) |
| `compaction_prune_protect_tokens` | `6000` | Most recent tool results (tokens) protected from pruning |

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

compaction_preserve_turns = 6
compaction_summarize = true
compaction_prune = true
compaction_prune_protect_tokens = 6000
```

## Agent behavior

The optional `[principal]` table.

| Key | Default | Meaning |
|-----|---------|---------|
| `principal.hard_stop_turns` | `0` | Hard-stop a turn after this many total tool turns. `0` = uncapped (the only execution cap; compaction is the backstop) |
| `principal.allow_model_stdin` | `false` | Whether the model may supply `stdin` bytes for a `bash` command it emits. Off by default: the bash schema exposes no `stdin` parameter and a command needing input either gets it from a human (interactive classifier → inline input panel) or fails fast with a non-interactive remedy hint (see ADR-0043). On: the bash schema dynamically adds a `stdin` field the model can fill, threaded through as a prefilled pipe — for unattended/automatic flows where no human is reachable |
| `principal.nudge.enabled` | `false` | Advanced doom-loop guard. When enabled, blocks a watched tool signature before its first repeat executes in the same turn. Forced off for envoys and `/review` |
| `principal.nudge.window` | `8` | Number of recent watched tool signatures retained for repeat detection |

```toml
[principal]
hard_stop_turns = 0
allow_model_stdin = false

# Advanced, opt-in deterministic repeated-call blocking. The `nudge` table
# name is retained for compatibility.
[principal.nudge]
enabled = false
window = 8
```

## Provider selection and retry

| Key | Default | Meaning |
|-----|---------|---------|
| `default_provider` | `""` (empty) | Provider id for the **fresh-session default**: used at startup and updated by a `/provider` switch so the next launch follows it; empty leaves the choice to the `/provider` picker. A switch also pins the selection to the session so resume restores it |
| `default_model` | `""` (empty) | Active model id within the selected provider, written by a `/provider` switch or add-provider flow alongside `default_provider`. The startup migration seeds a provider instance's default channel from it but no longer strips it |
| `provider_retry_max_attempts` | `6` | Max retry attempts for a transient provider error within a turn |
| `provider_retry_base_ms` | `1000` | Base delay for exponential backoff, in milliseconds |
| `provider_retry_max_ms` | `30000` | Cap on the backoff delay, in milliseconds |

## Built-in provider credentials and models

API keys accept an environment variable or an inline value; see
[Providers](providers.md) for the env-var names and capability matrix.

| Key | Default model | Purpose |
|-----|---------------|---------|
| `openai_api_key`, `openai_model` | `gpt-5.6-sol` | OpenAI |
| `gemini_api_key`, `gemini_model` | `gemini-3.5-flash` | Google Gemini |
| `moonshot_api_key`, `moonshot_model` | `k3` | Moonshot / Kimi Code |
| `deepseek_api_key`, `deepseek_flash_model`, `deepseek_pro_model` | `deepseek-v4-flash` / `deepseek-v4-pro` | DeepSeek V4 (shared key) |
| `zai_api_key`, `zai_model` | `glm-5.2` | Z.AI coding plan (GLM-5) |
| `anthropic_api_key`, `anthropic_model` | `claude-opus-4-8` | Anthropic |

## User-defined providers

`providers` is an array of `[[providers]]` tables, each with one or more
channels. A user entry whose `id` matches a built-in replaces it; otherwise it
adds a new model. See [Add a provider](../how-to/add-a-provider.md) for the
full schema and examples.

```toml
[[providers]]
id = "acme"
name = "Acme Relay"
default_channel = 0

  [[providers.channels]]
  label = "Default"
  transport = "OpenAiCompat"    # OpenAiCompat | Anthropic | GeminiNative
  model = "acme-7b"
  base_url = "https://relay.example.com/v1"
  api_key_env = "ACME_API_KEY"  # env var name; wins over api_key
  effort = "high"               # optional for OpenAI/Anthropic reasoning models
```

| `favorites` | Default | Meaning |
|-----|---------|---------|
| `favorites` | `[]` | Provider ids pinned for quick access in the picker |

## Per-model reasoning settings

Reasoning controls are **per model**, not per provider. `effort` is the
reasoning-depth throttle. `thinking` is an Anthropic-only on/off switch.

OpenAI GPT reasoning models use the channel-level `effort` field on
`OpenAiCompat` channels. Valid values are clamped to the model's supported
levels; GPT models can expose `none`, `minimal`, `low`, `medium`, `high`, and
`xhigh`.

Anthropic extended thinking is **opt-in** (ADR-0046). A model does not reason
unless you have configured it to. The two Anthropic knobs live in the
`[model_reasoning."<model-id>"]` table, keyed by model id, or on a
user-defined Anthropic channel.

**Opt-in rule:** a model's *presence* in this table opts it in to thinking.
Thinking defaults **on** (the recommended Claude mode) unless the entry
explicitly sets `thinking = false`; a set `effort` applies at that depth (else
the model's default, and `output_config` is omitted to keep the request lean). A
model **not** listed here sends no `thinking` object at all — it never reasons on
its own. Both fields are optional within an entry.

```toml
# Opus reasons at max depth (thinking on by default, since the entry exists).
[model_reasoning."claude-opus-4-8"]
effort   = "max"     # low | medium | high | xhigh | max (clamped to the model's levels)
# thinking omitted → defaults on

# Haiku: opted in but kept shallow and with thinking off.
[model_reasoning."claude-haiku-4-5"]
effort   = "low"
thinking = false
```

This table applies wherever the named Anthropic-format model is served — the
built-in `anthropic` provider and Anthropic-format relays alike. In the TUI,
drilling into a provider and pressing `e` on a model with reasoning controls
opens the per-model settings popup. OpenAI models show the Effort row.
Anthropic models show Effort plus the Thinking switch. For a user-defined
Anthropic relay, setting the channel's `effort` or `thinking` has the same
opt-in effect.

The legacy flat fields `anthropic_effort` / `anthropic_thinking` are
**deprecated** and no longer read — they only still load so an existing
`config.toml` does not break. Migrate by moving their values into a
`[model_reasoning]` entry.

## Quant runtime

`neenee-quant` reads a JSON config from `NEENEE_QUANT_CONFIG`, then applies
environment overrides. Missing values keep the defaults.

### Market data

| Environment variable | Default | Meaning |
|----------------------|---------|---------|
| `NEENEE_QUANT_MARKET_DATA` | `synthetic` | Market-data adapter: `synthetic`, `synthetic-paper`, `binance`, `binance-http`, `longport`, or `longbridge` |
| `NEENEE_QUANT_BINANCE_BASE_URL` | `https://api.binance.com` | Binance-compatible HTTP base URL |

### Broker

| Environment variable | Default | Meaning |
|----------------------|---------|---------|
| `NEENEE_QUANT_BROKER` | `paper` | Broker adapter: `paper`, `paper-trading`, `longport`, `longbridge`, or `live-http` |
| `NEENEE_QUANT_LIVE_BROKER_URL` | empty | HTTPS broker gateway base URL for `live-http`. Local development may use `http://localhost:*`, `http://127.0.0.1:*`, or `http://[::1]:*` |
| `NEENEE_QUANT_LIVE_BROKER_TOKEN_ENV` | `NEENEE_QUANT_LIVE_BROKER_TOKEN` | Environment variable that contains the live broker bearer token |
| `NEENEE_QUANT_LIVE_BROKER_TOKEN` | empty | Direct live broker bearer token override |

`live-http` never enables implicitly. It fails startup unless a non-empty
token and an accepted gateway URL are present.

The live broker gateway contract is:

| Method | Path | Request | Response |
|--------|------|---------|----------|
| `GET` | `/portfolio` | Optional `symbol` query parameter | `PortfolioSnapshot` JSON |
| `POST` | `/orders` | Order request plus `client_order_id` and `quote` | `OrderDecision` JSON |
| `POST` | `/orders/{order_id}/cancel` | `order_id` and `client_cancel_id` | `OrderDecision` JSON |

`neenee-quant` fetches `/portfolio` and applies local risk checks before
posting to `/orders`. A local risk rejection does not call the gateway.

### LongPort OpenAPI

Set both `NEENEE_QUANT_MARKET_DATA` and `NEENEE_QUANT_BROKER` to `longport`
for direct Longbridge quote and live-trading access through the official Rust
SDK. `longbridge` is an alias for the same adapter.
The live LongPort broker rejects any other market-data adapter so local risk
checks cannot use synthetic or broker-incompatible prices.

LongPort support is compiled only when `neenee-quant` enables its `longport`
feature. The GUI exposes it through the feature of the same name; launch the
live profile with `--features gui,longport`. Paper and backtest builds do not
compile the LongPort SDK.

| Environment variable | Default | Meaning |
|----------------------|---------|---------|
| `NEENEE_QUANT_LONGPORT_AUTH` | `apikey` | Authentication mode: `apikey` or `oauth` |
| `LONGPORT_APP_KEY` | empty | LongPort app key for `apikey`; environment only |
| `LONGPORT_APP_SECRET` | empty | LongPort app secret for `apikey`; environment only |
| `LONGPORT_ACCESS_TOKEN` | empty | LongPort access token for `apikey`; environment only |
| `NEENEE_QUANT_LONGPORT_OAUTH_CLIENT_ID` | empty | Registered OAuth client ID for `oauth` |
| `NEENEE_QUANT_LONGPORT_ACCOUNT_CURRENCY` | `USD` | Currency used for the live account summary and risk preflight |
| `LONGPORT_REGION` | automatic | Official SDK access-point override, such as `cn` or `hk` |

API-key credentials are never written to the JSON configuration or included
in debug output. OAuth prints the authorization URL on first use and delegates
token storage and refresh to the official SDK.

The adapter accepts LongPort security symbols such as `AAPL.US` and `700.HK`.
It does not add cryptocurrency trading to LongPort. Quote access still depends
on the account's market-data entitlements.

The adapter applies a minimum 20 ms interval and a 30-request rolling window
over LongPort trade calls. The official SDK controls quote-call frequency, but
LongPort leaves trade-call limiting to the client.

### Paper account, audit, and risk

| Environment variable | Default | Meaning |
|----------------------|---------|---------|
| `NEENEE_QUANT_PAPER_STARTING_CASH` | `100000` | Starting cash for the paper account |
| `NEENEE_QUANT_PAPER_COMMISSION_BPS` | `0` | Paper commission in basis points |
| `NEENEE_QUANT_PAPER_STATE` | empty | Optional JSON state file for persistent paper account state |
| `NEENEE_QUANT_AUDIT_LOG` | empty | Optional JSONL audit log for paper and live order decisions |
| `NEENEE_QUANT_RISK_MAX_ORDER_NOTIONAL` | `50000` | Per-order notional ceiling for paper and live brokers |
| `NEENEE_QUANT_RISK_MAX_GROSS_EXPOSURE` | `100000` | Gross exposure ceiling for paper and live brokers |
| `NEENEE_QUANT_RISK_ALLOW_SHORT_SELLING` | `false` | Whether paper or live sell orders may open short exposure |

## Intelligence workbench

The quant GUI composes `neenee-intelligence` with the trading runtime. It uses
existing global configuration rather than introducing a second provider or
web-search configuration surface.

| Capability | Configuration | State |
|------------|---------------|-------|
| Topic search | `[websearch]` | `intelligence/opinion.json` under XDG State |
| Link observation | `[websearch]` proxy and timeout | `intelligence/opinion.json` under XDG State |
| Expert council | `default_provider`, `default_model`, and the selected provider's credentials | `intelligence/expert-meetings.json` under XDG State |

The expert council is unavailable when no provider is configured. One meeting
makes 11 provider calls: five independent responses, five cross-examinations,
and one meeting-manager synthesis. The archive retains the 20 most recent
meetings.

Watched links accept public HTTP and HTTPS URLs. Each response is limited to
8 MiB. The archive retains a short text preview, validators, and a SHA-256
fingerprint rather than the full response body.

See [How to use the intelligence workbench](../how-to/use-intelligence-workbench.md)
for the operator workflow.

## TUI presentation

The optional `[tui]` table. Appearance and layout values can also be changed
interactively with `/config`.

| Key | Default | Meaning |
|-----|---------|---------|
| `tui.transcript_layout` | `"default"` | Transcript grouping: `default` (round bands) or `legacy` |
| `tui.color_scheme` | `"zen"` | Active palette: `zen`, `midnight`, `nord`, `catppuccin`, `paper`, or `custom` |
| `tui.default_expanded.<step>` | presenter default | Default expand state for a tool name or `thinking` |
| `tui.custom_color_scheme.background` | `"#070808"` | Terminal canvas |
| `tui.custom_color_scheme.surface` | `"#0e0f0f"` | Panels and menus |
| `tui.custom_color_scheme.text` | `"#d5d5cd"` | Primary foreground |
| `tui.custom_color_scheme.muted` | `"#777d75"` | Secondary foreground |
| `tui.custom_color_scheme.accent` | `"#8ea191"` | Focus and brand color |
| `tui.custom_color_scheme.success` | `"#759475"` | Positive states |
| `tui.custom_color_scheme.warning` | `"#b5955d"` | Caution states |
| `tui.custom_color_scheme.error` | `"#be6f68"` | Failure states |

The custom palette contains eight `#RRGGBB` semantic colors. The renderer
derives its remaining surfaces, hover states, code colors, and diff colors
from these values.

```toml
[tui]
transcript_layout = "default"
color_scheme = "custom"

[tui.default_expanded]
edit_file = true
bash = true
thinking = false

[tui.custom_color_scheme]
background = "#070808"
surface = "#0e0f0f"
text = "#d5d5cd"
muted = "#777d75"
accent = "#8ea191"
success = "#759475"
warning = "#b5955d"
error = "#be6f68"
```

## Hooks

Lifecycle event hooks (ADR-0025): each entry runs a shell command at one
point in the agent's lifecycle. See the [hooks explanation](../explanation/agent-design/hooks.md)
for the event set, the command contract, and how hooks compose with the
permission broker and the `/pursue` stop-gate.

The `[[hooks]]` array contains one table per hook. The capability a hook has
(block / inject / observe) is implied by its `event` — see the explanation for
which event honours which.

| Key | Default | Meaning |
|-----|---------|---------|
| `hooks[].event` | — | The lifecycle event: `SessionStart`, `SessionEnd`, `UserPromptSubmit`, `PreToolUse`, `PostToolUse`, `PostToolUseFailure`, `Stop`, `PreCompact`, `PostCompact`, `Turn` (ADR-0030, round end), `RoundStart` (round start), `PermissionRequest` (agent blocked on an approval prompt), `UserQuestion` (agent blocked on an `ask_user` question). `Turn`/`RoundStart` are `Deny`-forbidden (inject or observe only); `PermissionRequest`/`UserQuestion` are observe-only (fire-and-forget, ideal for desktop notifications). `PermissionRequest` honours a tool-name matcher |
| `hooks[].matcher` | `*` | Tool-name filter. A `|`-separated list of exact names (`Write|Edit`) when only letters/digits/`_`/`|`; otherwise a regular expression. Only the tool events honour it |
| `hooks[].command` | — | Shell command run when the event matches. Receives the event JSON on stdin; replies via exit code / stdout JSON |

```toml
[[hooks]]
event   = "PostToolUse"
matcher = "Write|Edit"
command = ".neenee/hooks/lint.sh"

[[hooks]]
event   = "PreToolUse"
matcher = "Bash"
command = ".neenee/hooks/guard-rm.sh"

[[hooks]]
event   = "Stop"
command = ".neenee/hooks/ci-gate.sh"

# ADR-0030: fires once per tool round, at round end. Deny is ignored (no
# de-facto round cap); inject context or observe. Carries the read-only-round
# streak.
[[hooks]]
event   = "Turn"
command = ".neenee/hooks/turn-watch.sh"

# Symmetric partner: fires at the *start* of each tool round, after tools are
# prepared but before the next model completion. Use it to (re)inject context at
# the top of the model's attention for the round — e.g. to re-anchor the
# principal's role after a run of read-only delegations. Deny is ignored here
# too.
[[hooks]]
event   = "RoundStart"
command = ".neenee/hooks/round-open.sh"

# Interrupt notifications (observe-only): fire-and-forget when the agent blocks
# waiting for you. The canonical use is a desktop/bell notification so a
# long-running task that goes unattended still gets your attention. Outcomes are
# ignored — these never grant/deny or alter the transcript. The matcher targets
# the tool seeking approval (here: only bash). `UserQuestion` has no matcher.
[[hooks]]
event   = "PermissionRequest"
matcher = "bash"
command = ".neenee/hooks/notify.sh \"Needs approval\""
[[hooks]]
event   = "UserQuestion"
command = ".neenee/hooks/notify.sh \"AI asked a question\""
```

## Feature tables

These sub-tables have their own reference pages; only the table name is
configured here.

| Table | Configures | Reference |
|-------|------------|-----------|
| `[skills]` | Skill sources, extra paths, disabled skills | [Skills](tools/skills.md) |
| `[websearch]` | Web-search backend, proxy, timeout | [Web tool](tools/web.md) |
| `[mcp.<server>]` | MCP servers (one table per server) | [MCP](tools/mcp.md) |
