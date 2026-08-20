# Paths

Where neenee reads and writes files. Lookup-oriented: for the conceptual
model, see [Persistence and the XDG layout](../explanation/persistence.md);
for the durable policy, see [ADR-0014](../adr/0014-xdg-persistence-architecture.md).

## Override precedence

Each XDG category resolves through the same fixed precedence, highest first.

| # | Source | Notes |
|---|--------|-------|
| 1 | `--home <dir>` | Instance root (ADR-0121): the CLI form of the `NEENEE_HOME` selector; wins over the env var |
| 2 | `NEENEE_CONFIG_DIR`, `NEENEE_DATA_DIR`, `NEENEE_STATE_DIR`, `NEENEE_CACHE_DIR` | App-specific env override; more specific than the root, so one category can be carved out of a sandbox |
| 3 | `NEENEE_HOME` | Instance root (ADR-0121): `<dir>/neenee/{config,data,state,cache}` + `<dir>/neenee/instance` for daemon runtime files. One variable isolates the entire footprint — the dev/test sandbox shape. Relative or empty values are ignored |
| 4 | `XDG_CONFIG_HOME`, `XDG_DATA_HOME`, `XDG_STATE_HOME`, `XDG_CACHE_HOME` | Standard XDG env override; relative values ignored per spec |
| 5 | Native per-OS default | `directories` crate: `~/.config` etc. on Linux, `~/Library/Application Support` on macOS, `%APPDATA%` on Windows |
| 6 | `$HOME/.config`, `$HOME/.local/share`, `$HOME/.local/state`, `$HOME/.cache` | Spec default when nothing else applies |
| 7 | Current working directory | Last resort; never panics |

All four categories honour the same stack — no per-subsystem special cases.
The instance root sits *below* the per-category variables (specific beats
general) and *above* the `XDG_*` layer, so one sandbox switch wins over the
ambient desktop environment.

The daemon runtime files resolve through the same idea, terminated by
[`instance_dir`]: `--home`/`NEENEE_HOME` (`<dir>/neenee/instance`) >
`$XDG_RUNTIME_DIR/neenee` > data dir fallback. `NEENEE_PORT` is the
port-layer sibling: it overrides the well-known 9800 default (an explicit
`--port` still wins).

## Config — `$XDG_CONFIG_HOME/neenee/`

User-edited configuration. Lossy; back it up.

| Path | Purpose | Lossy? |
|------|---------|--------|
| `config.toml` | User-edited configuration — **behavior only** (`default_provider` / `default_model`, `[principal]`, `[permissions]`, `[bash_policy]`, `[tui]`, `[input_history]`, `[tool_variants]`, `[[hooks]]`, `[skills]`, `[websearch]`, `[mcp.<server>]`, ...). Provider *instances* live in the state store (`providers.toml`), secrets in `credentials.toml` | Yes |
| `credentials.toml` | Token-auth secrets, split out of `config.toml` (written `rw-------`), keyed by **provider instance**: `[providers.<id>] api_key`. OAuth logins do not live here — see the note below. A credential belongs to the instance; every route it serves resolves it. | Yes |
| `logo.txt` | Optional user-supplied ASCII logo; when present its lines replace the built-in wordmark on the welcome screen | Rebuildable |

Default location: `~/.config/neenee/`.

OAuth token sets (`auth.toml`, 0600) are **runtime state, not user
config** — they live under `$XDG_STATE_HOME/neenee/auth.toml` (see the
State section below). A legacy `~/.config/neenee/auth.toml` from older
releases is still read as a fallback (`legacy_auth_file`) and migrated on
first save.

The two credential kinds, side by side:

| Kind | File | Keyed by | Contents |
|------|------|----------|----------|
| token (API key) | `~/.config/neenee/credentials.toml` | provider instance (`[providers.<id>]`) | `api_key` |
| oauth (subscription login) | `~/.local/state/neenee/auth.toml` | provider instance (`[tokens.<provider>]`) | `access` / `refresh` / `expires_ms` / `account_id` |

The category split follows the XDG spec's own test ("important or portable
enough to the user?") rather than the fact that both files hold secrets: a
user-supplied API key is important, portable, and hand-editable — config —
while an OAuth token set is daemon-rewritten on every refresh and recoverable
by re-login — state. Both files are 0600 with private temp writes, so the
placement buys no security either way. Rationale and alternatives:
[ADR-0115](../adr/0115-credential-placement-config-vs-state.md).

## Data — `$XDG_DATA_HOME/neenee/`

Persistent, program-generated, must survive restart. Back it up.

| Path | Purpose | Lossy? |
|------|---------|--------|
| `blobs/<2-char-prefix>/<hash>` | Content-addressed blob store for large payloads | Yes |
| `projects/<16-hex-bucket>/` | Per-project bucket: sessions, current pointer, metadata | Yes |
| `projects/<bucket>/sessions/` | Durable per-session files: `sessions/<id>.json` (snapshot) plus `sessions/<id>.jsonl` (event log). Each live instance pins its own file, so concurrent instances never share a mutable one | Yes |
| `projects/<bucket>/network/` | Per-project `/debug trace` capture directory (mirrors the `sessions/` layout) | Rebuildable |
| `projects/<bucket>/debug/` | Per-project `/debug preview` capture directory (one owner-only JSON per invocation) | Rebuildable |
| `projects/<bucket>/embeddings.json` | Per-project lightweight embedding index | Rebuildable (re-indexed) |
| `projects/<bucket>/neenee.lock` | Per-project advisory lock | Rebuildable |
| `projects/<bucket>/permissions.json` | Per-project cached "always allow" permission rules | Rebuildable (re-prompts) |
| `usage/daily/<YYYY-MM-DD>.json` | Cross-session usage statistics (ADR-0122): one append-only file per local day of terminal request records, mirrored from the token ledger. A **sibling of `projects/`**, so session cleanup never touches it; powers the `/usage` overlay | Yes (history is unrecoverable) |
| `skills/` | User-global skills (`SKILL.md` per skill) | Yes (user-authored) |
| `commands/` | User-global slash commands | Yes (user-authored) |

Default location: `~/.local/share/neenee/`.

The per-project bucket is `sha256(cwd)[..16]` — 16 hex chars (64 bits),
ASCII-safe, not reversible to the cwd from the path alone.

## State — `$XDG_STATE_HOME/neenee/`

Persistent, program-generated, rebuildable. Loss flattens sort order or
re-prompts; no conversation is lost.

| Path | Purpose | Lossy? |
|------|---------|--------|
| `history.json` | Slash-command input history | Rebuildable |
| `providers.toml` | **Provider instances** — the program-managed "who I connect to" records: id/name, `template_id`, `auth`, optional `api_key_env`, and a pure-custom instance's declared transport/endpoint/models. Deliberately NOT in `config.toml`, which holds behavior only; routes are derived at runtime from each instance's template + the discovery cache, never persisted | No (user-managed connections) |
| `trusted_projects.json` | The per-project trust grant set (which projects' `.neenee/config.toml` external tools are loaded) | Rebuildable (re-trust) |
| `provider_usage.json` | Per-model usage telemetry driving recency sort in the model picker | Rebuildable |
| `model_usage.json` | Per-model token usage telemetry | Rebuildable |
| `auth.toml` | OAuth token sets per provider id (`[tokens.<provider>]`, 0600) — access/refresh/expiry for SuperGrok, ChatGPT, Copilot, and Google Antigravity logins. Rebuildable only by re-logging in (the refresh tokens are the durable secret; losing the file means re-auth, so back it up if rotating logins is costly) | Re-auth on loss |
| `neenee.lock` | Cross-process advisory lock when no runtime directory is available | Rebuildable |
| `log/` | Structured rolling-log appender output (reserved) | Rebuildable |

Default location: `~/.local/state/neenee/`.

## Cache — `$XDG_CACHE_HOME/neenee/`

Derived, deletable, repopulated on demand. Safe to delete.

| Path | Purpose | Lossy? |
|------|---------|--------|
| `skills/remote/` | Cached remote skill repositories (fetched from `[skills] urls`) | Safe to delete |
| `models_discovery.json` | Per-route facts derived from live `GET /models`: the discovered model list, fitted capability metadata, and the user's per-(instance, model) reasoning overrides (`route_settings`) | Rebuildable (re-discovered); `route_settings` is user-set and recreated by the model `e` editor |

Default location: `~/.cache/neenee/`.

## Runtime — the daemon instance dir

Ephemeral per daemon. By default `$XDG_RUNTIME_DIR/neenee/` on Linux;
moved wholesale to `<dir>/neenee/instance` by the instance root selector
(`--home` / `NEENEE_HOME`, ADR-0121). Never assume the default location
exists.

| Path | Purpose | Lossy? |
|------|---------|--------|
| `daemon.lock` | Cross-process single-instance `flock` | Ephemeral |
| `daemon.json` | Unified session-daemon discovery record (pid, TCP port, UDS path, token when exposed, daemon `version`); written on startup after the port is bound, removed on every shutdown path — graceful, forced, or panic (ADR-0096/0101) | Ephemeral |
| `daemon.sock` | The daemon's Unix-domain control-plane socket (0600); removed on shutdown (ADR-0096) | Ephemeral |
| `serve/<bucket>.json` | Legacy pre-ADR-0096 per-project discovery records; ignored by current clients (harmless litter) | Ephemeral |

Without any override, the daemon falls back to the data directory for these
files when `$XDG_RUNTIME_DIR` is unset.

## Project working tree (not under XDG)

Lives with the project root; travels with the repository.

| Path | Purpose |
|------|---------|
| `.neenee/skills/<name>/SKILL.md` | Project-local skills (highest discovery priority) |
| `.neenee/commands/<name>.md` | Project-local slash commands (highest discovery priority) |
| `.neenee/config.toml` | Project-scope configuration (MCP servers + hooks); loaded only after the project is trusted (ADR-0085) |
| `session.json`, `events.jsonl` | Legacy in-project session storage at the project root (transitional; superseded by `projects/<bucket>/sessions/`) |
| `.agents/skills/`, `.claude/skills/` | External application conventions (read-only) |
| `.agents/commands/` | External application conventions (read-only) |

The project root is located by walking upward from the current directory
looking for the first ancestor containing `.neenee`, `.git`, `Cargo.toml`,
or `package.json`.

## macOS and Windows defaults

The `directories` crate provides native defaults on non-Linux platforms.
The override stack is identical; only the fallback locations differ.

| Category | macOS | Windows |
|----------|-------|---------|
| Config | `~/Library/Application Support/neenee` | `%APPDATA%\neenee\config` |
| Data | `~/Library/Application Support/neenee` | `%APPDATA%\neenee\data` |
| State | `~/Library/Application Support/state` (no native state dir on macOS; falls back to the data dir's sibling `../state`) | `%LOCALAPPDATA%\neenee\state` |
| Cache | `~/Library/Caches/neenee` | `%LOCALAPPDATA%\neenee\cache` |

`XDG_*_HOME` env vars still take precedence over these on every platform.

## Isolated instances (development and testing)

`--home <dir>` (or `NEENEE_HOME=<dir>`) gives neenee a fully separate
footprint: config, credentials, sessions, skills, logs, the daemon's
socket/lock/discovery record, and (via `NEENEE_PORT`) the default TCP port.
A sandboxed client spawns its on-demand daemon with the inherited
environment, so the daemon lands in the same sandbox — the host
installation's daemon and data are never touched.

| Purpose | Command |
|---------|---------|
| Run one command isolated | `neenee --home /tmp/x <args>` |
| Isolate a whole shell / CI step | `export NEENEE_HOME=/tmp/x NEENEE_PORT=9801` |
| Run the test suites isolated | `export NEENEE_HOME=$(mktemp -d)` then `cargo test` |
| Confirm which instance a client sees | `neenee --home /tmp/x daemon status --diagnostic` |

See [ADR-0121](../adr/0121-instance-isolation-for-development-and-testing.md)
for the decision record.

## Cleanup quick reference

| Purpose | Command |
|------|---------|
| Reset caches | `rm -rf $XDG_CACHE_HOME/neenee` |
| Reset rebuildable state | `rm -rf $XDG_STATE_HOME/neenee` |
| Reset one project's history | `rm -rf $XDG_DATA_HOME/neenee/projects/<bucket>` |
| Factory reset (keep config) | `rm -rf $XDG_DATA_HOME/neenee $XDG_STATE_HOME/neenee $XDG_CACHE_HOME/neenee` |
| Full wipe (including config) | Add `rm -rf $XDG_CONFIG_HOME/neenee` to the above |

## Legacy stray files

Files written by releases whose subsystems no longer exist. Safe to delete;
listed here because they may still sit in an older installation's
directories:

| File | Written by | Removed by |
|------|-----------|------------|
| `$XDG_CONFIG_HOME/neenee/goals.db` | the pre-ADR-0082 goal scheduler (SQLite) | any release after the SQLite removal |
| `$XDG_CONFIG_HOME/neenee/session.json` | the pre-ADR-0096 single-session layout | ADR-0096 (per-project buckets) |
| `$XDG_STATE_HOME/neenee/model_usage.json` | the pre-ADR-0024 usage telemetry (SQLite era) | ADR-0024's supersession; `provider_usage.json` is the live file |
| `$XDG_DATA_HOME/neenee/repeat.db` | the pre-ADR-0082 `/repeat` scheduler (SQLite) | any release after the SQLite removal |
| `$XDG_CACHE_HOME/neenee/models-dev.json` | an older remote-models cache format | the discovery cache (`models_discovery.json`) replaced it |

None of these are read by the current code; deleting them changes nothing at
runtime. They are *not* removed automatically — a tool silently deleting
files from a user's home directory is worse than a stale file.
