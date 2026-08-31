# Paths

Where muta reads and writes files. Lookup-oriented: for the conceptual
model, see [Platform-native persistence categories](../explanation/persistence.md);
for the durable policy, see [ADR-0014](../adr/0014-xdg-persistence-architecture.md).

## Override precedence

Each semantic category resolves through the same fixed precedence, highest
first. XDG variables are Linux-native and remain portable explicit overrides;
native defaults are used when they are absent.

| # | Source | Notes |
|---|--------|-------|
| 1 | `MUTA_CONFIG_DIR`, `MUTA_DATA_DIR`, `MUTA_STATE_DIR`, `MUTA_CACHE_DIR` | App-specific env override; more specific than the root, so one category can be carved out of a sandbox |
| 2 | `MUTA_HOME` | Instance root (ADR-0121): `<dir>/muta/{config,data,state,cache}` + `<dir>/muta/instance` for daemon runtime files. One variable isolates the entire footprint — the dev/test sandbox shape. Relative or empty values are ignored |
| 3 | `XDG_CONFIG_HOME`, `XDG_DATA_HOME`, `XDG_STATE_HOME`, `XDG_CACHE_HOME` | Standard XDG env override; relative values ignored per spec |
| 4 | Native per-OS default | `directories` crate: XDG defaults on Linux, `~/Library/Application Support` on macOS, `%APPDATA%` / `%LOCALAPPDATA%` on Windows |
| 5 | `$HOME/.config`, `$HOME/.local/share`, `$HOME/.local/state`, `$HOME/.cache` | Unix-only last-resort default when native resolution is unavailable |
| 6 | Current working directory | Last resort; never panics |

All four categories honour the same stack — no per-subsystem special cases.
The instance root sits *below* the per-category variables (specific beats
general) and *above* the `XDG_*` layer, so one sandbox switch wins over the
ambient desktop environment.

The daemon runtime files resolve through the same idea, terminated by
[`instance_dir`]: `MUTA_HOME` (`<dir>/muta/instance`) >
`$XDG_RUNTIME_DIR/muta` > the native fallback (data on Unix,
`%LOCALAPPDATA%\muta\state\instance` on Windows). `MUTA_PORT` is the
port-layer sibling: it overrides the well-known 9800 default (an explicit
`--port` still wins).

## Config — `$XDG_CONFIG_HOME/muta/`

User-edited configuration. Lossy; back it up.

| Path | Purpose | Lossy? |
|------|---------|--------|
| `config.toml` | User-edited configuration — **daemon & core behavior only** (`default_connection` / `default_model`, `[compaction]`, `[permissions]`, `[workspace]`, `[bash_policy]`, `[tool_variants]`, `[[hooks]]`, `[skills]`, `[websearch]`, `[mcp.<server>]`, `[daemon]`, `[master]`, ...). Connection *instances* live in `connections.toml`, secrets in `credentials.toml` | Yes |
| `credentials.toml` | Token-auth secrets, split out of `config.toml` (written `rw-------`), keyed by **connection instance**: `[connections.<id>] api_key`. OAuth logins do not live here — see the note below. | Yes |

Default location: `~/.config/muta/`.

OAuth token sets (`auth.toml`, 0600) are **runtime state, not user
config** — they live under `$XDG_STATE_HOME/muta/auth.toml` (see the
State section below). A legacy `~/.config/muta/auth.toml` from older
releases is still read as a fallback (`legacy_auth_file`) and migrated on
first save.

The two credential kinds, side by side:

| Kind | File | Keyed by | Contents |
|------|------|----------|----------|
| token (API key) | `~/.config/muta/credentials.toml` | provider instance (`[providers.<id>]`) | `api_key` |
| oauth (subscription login) | `~/.local/state/muta/auth.toml` | provider instance (`[tokens.<provider>]`) | `access` / `refresh` / `expires_ms` / `account_id` |

The category split follows the XDG spec's own test ("important or portable
enough to the user?") rather than the fact that both files hold secrets: a
user-supplied API key is important, portable, and hand-editable — config —
while an OAuth token set is daemon-rewritten on every refresh and recoverable
by re-login — state. Both files are 0600 with private temp writes, so the
placement buys no security either way. Rationale and alternatives:
[ADR-0115](../adr/0115-credential-placement-config-vs-state.md).

## Data — `$XDG_DATA_HOME/muta/`

Persistent, program-generated, must survive restart. Back it up.

| Path | Purpose | Lossy? |
|------|---------|--------|
| `blobs/<2-char-prefix>/<hash>` | Content-addressed blob store for large payloads. Unreferenced blobs are reclaimed by the daemon's daily mark-sweep GC (sessions across all project buckets are the roots) | Yes |
| `projects/<16-hex-bucket>/` | Per-project bucket: sessions, current pointer, metadata | Yes |
| `projects/<bucket>/sessions/` | Durable per-session files: `sessions/<id>.json` (snapshot) plus `sessions/<id>.jsonl` (event log). Each live instance pins its own file, so concurrent instances never share a mutable one | Yes |
| `projects/<bucket>/network/` | Per-project `/debug trace` capture directory; bounded retention (newest 50 captures kept — each is a full context, so they grow fast) | Rebuildable |
| `projects/<bucket>/debug/` | Per-project `/debug preview` capture directory (one owner-only JSON per invocation, same bounded retention) | Rebuildable |
| `projects/<bucket>/embeddings.json` | Per-project lightweight embedding index | Rebuildable (re-indexed) |
| `projects/<bucket>/muta.lock` | Per-project advisory lock | Rebuildable |
| `projects/<bucket>/permissions.json` | Per-project cached "always allow" permission rules | Rebuildable (re-prompts) |
| `usage/daily/<YYYY-MM-DD>.json` | Cross-session usage statistics (ADR-0122): one append-only file per local day of terminal request records, mirrored from the token ledger. A **sibling of `projects/`**, so session cleanup never touches it; powers the `/usage` overlay | Yes (history is unrecoverable) |
| `skills/` | User-global skills (`SKILL.md` per skill) | Yes (user-authored) |
| `commands/` | User-global slash commands | Yes (user-authored) |

Default location: `~/.local/share/muta/`.

The per-project bucket is `sha256(cwd)[..16]` — 16 hex chars (64 bits),
ASCII-safe, not reversible to the cwd from the path alone.

## State — `$XDG_STATE_HOME/muta/`

Persistent, program-generated, rebuildable. Loss flattens sort order or
re-prompts; no conversation is lost.

| Path | Purpose | Lossy? |
|------|---------|--------|
| `history.json` | Slash-command input history | Rebuildable |
| `providers.toml` | **Connections** — the program-managed "who I connect to" records: id/name, `preset_id`, `auth`, optional `api_key_env`, and a pure-custom connection's declared transport/endpoint/models. Deliberately NOT in `config.toml`, which holds behavior only; routes are derived at runtime from each connection's preset + the discovery cache, never persisted | No (user-managed connections) |
| `route_settings.json` | The user's per-(instance, model) reasoning overrides — set from the model `e` editor. State, not cache: deleting it loses user configuration no endpoint can re-derive (migrated out of `models_discovery.json`) | No |
| `workspace_security.json` | Versioned, canonical-workspace-keyed SHA-256 grants for the concrete `mcp`, `skills`, `hooks`, `rules`, and `roots` project asset domains | Rebuildable (project asset trust must be granted again) |
| `provider_usage.json` | Per-model usage telemetry driving recency sort in the model picker | Rebuildable |
| `auth.toml` | OAuth token sets per provider id (`[tokens.<provider>]`, 0600) — access/refresh/expiry for SuperGrok, ChatGPT, Copilot, and Google Antigravity logins. Rebuildable only by re-logging in (the refresh tokens are the durable secret; losing the file means re-auth, so back it up if rotating logins is costly) | Re-auth on loss |
| `muta.lock` | Cross-process advisory lock when no runtime directory is available | Rebuildable |
| `log/` | Structured rolling-log appender output, daily rotation with bounded retention (`MUTA_LOG_RETENTION`, default 14 files) | Rebuildable |

Default location: `~/.local/state/muta/`.

## Mutx Terminal App Paths — `$XDG_CONFIG_HOME/mutx/`, `$XDG_STATE_HOME/mutx/`

Following ADR-0136, the `mutx` terminal frontend's user preferences, themes, and prompt history are fully decoupled from the core daemon.

| Path | Location | Purpose | Lossy? |
|------|----------|---------|--------|
| `config.toml` | `$XDG_CONFIG_HOME/mutx/config.toml` | Terminal presentation preferences (`color_scheme`, `transcript_layout`, `[custom_color_scheme]`, `[default_expanded]`, `[input_history]`, …) | Yes |
| `themes/*.toml` | `$XDG_CONFIG_HOME/mutx/themes/` | Standalone custom theme files; discovered dynamically by `/config` › Appearance | User-authored |
| `logo.txt` | `$XDG_CONFIG_HOME/mutx/logo.txt` | Optional user-supplied ASCII logo for the welcome screen | Rebuildable |
| `history.json` | `$XDG_STATE_HOME/mutx/history.json` | Persisted prompt input history and `Ctrl+R` recall index | Rebuildable |

Default locations: `~/.config/mutx/` and `~/.local/state/mutx/`.


## Cache — `$XDG_CACHE_HOME/muta/`

Derived, deletable, repopulated on demand. Safe to delete.

| Path | Purpose | Lossy? |
|------|---------|--------|
| `skills/remote/` | Cached remote skill repositories (fetched from `[skills] urls`) | Safe to delete |
| `models_discovery.json` | Per-route facts derived from live `GET /models`: the discovered model list and fitted capability metadata. All derived — wiping the file costs one re-discovery | Rebuildable |

Default location: `~/.cache/muta/`.

## Runtime — the daemon instance dir

Ephemeral per daemon. By default `$XDG_RUNTIME_DIR/muta/` on Linux;
moved wholesale to `<dir>/muta/instance` by the instance root selector
(`--home` / `MUTA_HOME`, ADR-0121). Never assume the default location
exists.

| Path | Purpose | Lossy? |
|------|---------|--------|
| `daemon.lock` | Cross-process single-instance lock (`flock` on Unix, `LockFileEx` on Windows) | Ephemeral |
| `daemon.json` | Unified session-daemon discovery record (pid, TCP port, native local endpoint, token when exposed, daemon `version`); written on startup after the endpoints bind and removed on every shutdown path | Ephemeral |
| `daemon.sock` | Unix only: owner-only Unix-domain control-plane socket; listener drop removes it, and the next lock-owning bind safely replaces state left by a killed daemon | Ephemeral |
| `\\.\pipe\muta-<user-sid>-daemon-<instance-hash>` | Windows only: instance-isolated Named Pipe with a protected DACL granting the current user and LocalSystem | Ephemeral |
| `serve/<bucket>.json` | Legacy pre-ADR-0096 per-project discovery records; ignored by current clients (harmless litter) | Ephemeral |

Without an instance/runtime override, macOS and Linux fall back to the data
directory; Windows uses `%LOCALAPPDATA%\muta\state\instance` so ephemeral
coordination never roams with the user profile.

## Project working tree (not under XDG)

Lives with the project root; travels with the repository.

| Path | Purpose |
|------|---------|
| `.muta/skills/<name>/SKILL.md` | Project-local skills (highest discovery priority) |
| `skills/<name>/SKILL.md` | Top-level project-local skill convention |
| `.muta/commands/<name>.md` | Project-local slash commands (highest discovery priority) |
| `.muta/mcp.json` | Project MCP definitions; loaded only while the MCP asset domain is trusted |
| `.muta/hooks/` | Project hook scripts included in the Hooks-domain digest |
| `.muta/config.toml` | Project-scope MCP and hook definitions; each narrow table projection is loaded only while its own domain is trusted. A project `[workspace]` table cannot widen filesystem admission |
| `session.json`, `events.jsonl` | Legacy in-project session storage at the project root (transitional; superseded by `projects/<bucket>/sessions/`) |
| `.agents/skills/`, `.claude/skills/` | External application conventions (read-only) |
| `.agents/commands/` | External application conventions (read-only) |

The project root is located by walking upward from the current directory
looking for the first ancestor containing `.muta`, `.git`, `Cargo.toml`,
or `package.json`.

## macOS and Windows defaults

The `directories` crate provides native defaults on non-Linux platforms.
The override stack is identical; only the fallback locations differ.

| Category | macOS | Windows |
|----------|-------|---------|
| Config | `~/Library/Application Support/muta` | `%APPDATA%\muta\config` |
| Data | `~/Library/Application Support/muta` | `%APPDATA%\muta\data` |
| State | `~/Library/Application Support/muta/state` (macOS has no separate native state root) | `%LOCALAPPDATA%\muta\state` |
| Cache | `~/Library/Caches/muta` | `%LOCALAPPDATA%\muta\cache` |

On Windows, daemon discovery and lock records fall back to
`%LOCALAPPDATA%\muta\state\instance`; they never enter the roaming profile.
`XDG_*_HOME` env vars remain accepted as explicit cross-platform overrides.
They do not replace these native defaults merely because the process runs on
Windows or macOS.

## Isolated instances (development and testing)

`MUTA_HOME=<dir>` gives both Muta command surfaces a fully separate
footprint: config, credentials, sessions, skills, logs, the daemon's native
endpoint/lock/discovery record, and (via `MUTA_PORT`) the default TCP port.
A sandboxed client spawns its on-demand daemon with the inherited
environment, so the daemon lands in the same sandbox — the host
installation's daemon and data are never touched.

| Purpose | Command |
|---------|---------|
| Run one terminal command isolated | `MUTA_HOME=/tmp/x mutx <args>` |
| Isolate a whole shell / CI step | `export MUTA_HOME=/tmp/x MUTA_PORT=9801` |
| Run the test suites isolated | `export MUTA_HOME=$(mktemp -d)` then `cargo nextest run` |
| Confirm which instance a client sees | `MUTA_HOME=/tmp/x muta daemon status --diagnostic` |

See [ADR-0121](../adr/0121-instance-isolation-for-development-and-testing.md)
for the decision record.

## Cleanup quick reference

| Purpose | Command |
|------|---------|
| Reset caches | `rm -rf $XDG_CACHE_HOME/muta` |
| Reset rebuildable state | `rm -rf $XDG_STATE_HOME/muta` |
| Reset one project's history | `rm -rf $XDG_DATA_HOME/muta/projects/<bucket>` |
| Factory reset (keep config) | `rm -rf $XDG_DATA_HOME/muta $XDG_STATE_HOME/muta $XDG_CACHE_HOME/muta` |
| Full wipe (including config) | Add `rm -rf $XDG_CONFIG_HOME/muta` to the above |

## Legacy stray files

Files written by releases whose subsystems no longer exist. Safe to delete;
listed here because they may still sit in an older installation's
directories:

| File | Written by | Removed by |
|------|-----------|------------|
| `$XDG_CONFIG_HOME/muta/goals.db` | the pre-ADR-0082 goal scheduler (SQLite) | any release after the SQLite removal |
| `$XDG_CONFIG_HOME/muta/session.json` | the pre-ADR-0096 single-session layout | ADR-0096 (per-project buckets) |
| `$XDG_STATE_HOME/muta/model_usage.json` | the pre-ADR-0024 usage telemetry (SQLite era) | ADR-0024's supersession; `provider_usage.json` is the live file |
| `$XDG_DATA_HOME/muta/repeat.db` | the pre-ADR-0082 `/repeat` scheduler (SQLite) | any release after the SQLite removal |
| `$XDG_CACHE_HOME/muta/models-dev.json` | an older remote-models cache format | the discovery cache (`models_discovery.json`) replaced it |

None of these are read by the current code; deleting them changes nothing at
runtime. They are *not* removed automatically — a tool silently deleting
files from a user's home directory is worse than a stale file.
