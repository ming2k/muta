# Platform-native persistence categories

muta writes a lot to disk: conversations, file blobs, embeddings,
telemetry, advisory locks, cached skills. The question "where does this
file live, what am I allowed to do with it, and what happens if I delete
it" must have one answer per file — and that answer must be derivable from
the file's *nature*, not looked up in a table every time.

This page is the conceptual model. For the per-file lookup, see
[Paths reference](../reference/paths.md). For the durable decision record,
see [ADR-0014](../adr/0014-xdg-persistence-architecture.md). For the shape of a
recoverable coding-agent session inside the data category, see
[Session persistence](agent-design/session-persistence.md).

## Why categories are platform-neutral

The [XDG Base Directory Specification](https://specifications.freedesktop.org/basedir-spec/latest/)
provides a strong semantic split for user-level files with **different
operational lifetimes**. muta adopts those meanings without treating Linux
paths as a universal filesystem convention. Linux uses XDG locations, macOS
uses Application Support and Caches, and Windows uses Roaming and Local App
Data. Explicit `XDG_*` variables remain portable overrides.

The historical alternative — one monolithic `~/.muta/` directory — had
three problems in practice:

1. **Backup blur.** Configuration, conversations, and rebuildable caches
   sat together. Backing up "muta" meant either too much (caches) or
   too little (only config).
2. **Cleanup ambiguity.** Nothing was safe to delete without reading the
   code to find out whether it would regenerate.
3. **Override impossibility.** Containerised, sandboxed, and multi-prefix
   setups had no knob short of `HOME=`, which is far too coarse.

## The four categories

muta classifies every file by **what it is**, then routes it to the
matching XDG category.

### Config — files the user edits by hand

On Linux, `$XDG_CONFIG_HOME/muta/` (default `~/.config/muta/`).

`config.toml` is the hand-edited configuration, and `credentials.toml`
(0600) is its secret half: API keys split out so the config file can be
shared or version-controlled without leaking them. OAuth token sets do
**not** live here — they are runtime state (see State below). Losing
either file is lossy: it captures user preferences, provider setup, and
keys. Restoring from backup is the right move.

Why API keys are *config* and not *state*: the XDG spec defines
`$XDG_STATE_HOME` for data "not important or portable enough to the user"
— histories, layouts, undo stacks. A user-supplied key is the opposite on
every axis: important (re-collecting it means visiting every provider),
portable (a new machine wants it), and manually editable by design (the
load path resolves `env > credentials.toml > config inline`). This is also
where the ecosystem puts them — cargo, gh, gcloud, aws, npm, kubectl,
docker. Security is orthogonal to the category: both directories get the
same 0600 treatment, so moving a secret to State would hide a
user-editable input inside a program-owned directory and buy nothing.
See [ADR-0115](../adr/0115-credential-placement-config-vs-state.md).

### Data — persistent, program-generated, must survive restart

On Linux, `$XDG_DATA_HOME/muta/` (default `~/.local/share/muta/`).

Conversations, content-addressed blobs, per-project
embedding indices, cached permission approvals, and user-authored skills
and commands. This is the irreplaceable history of the work the user has
done. Back it up.

The per-project bucket (under `projects/<short-hash>/`) keeps each
working directory's history isolated — different projects never see each
other's sessions. The hash is short (16 hex chars, 64 bits) to keep
names readable while keeping accidental collision across a single user's
projects astronomically unlikely.

### State — persistent, program-generated, rebuildable

On Linux, `$XDG_STATE_HOME/muta/` (default `~/.local/state/muta/`).

Slash-command history, per-model usage telemetry that orders the provider
picker by recency, advisory lock files when no runtime directory is
available, and `auth.toml` (0600) — the OAuth access/refresh token sets
per provider login. Loss is non-fatal: it flattens sort order or forces a
re-prompt, but no conversation or skill is lost; losing `auth.toml` costs
a re-login per OAuth provider, nothing more.

`auth.toml` is state rather than config *because the program owns its
lifecycle*: the daemon rewrites it on every token refresh, its contents
are ephemeral-derived (access tokens expire in ~1h), and its loss is
recoverable by re-login rather than by user re-collection. A
credentials *backup/export* feature must still include it alongside
`credentials.toml` — the refresh token is the durable secret of an OAuth
login — the categories describe ownership and churn, not backup-worthiness
([ADR-0115](../adr/0115-credential-placement-config-vs-state.md)).

### Cache — derived, deletable, repopulated on demand

On Linux, `$XDG_CACHE_HOME/muta/` (default `~/.cache/muta/`).

The remote-skill cache. Safe to delete at any time; the next startup
that needs a remote skill fetches it again. Treat as ephemeral.

### Runtime — ephemeral per daemon

`$XDG_RUNTIME_DIR/muta/` when the variable is set.

On Linux, `$XDG_RUNTIME_DIR` holds ephemeral discovery and lock files when it
is available. macOS uses its application-data fallback; Windows keeps the
records in machine-local state rather than the roaming profile. Local control
traffic follows native access boundaries: Unix uses a domain socket, while
Windows uses a per-user Named Pipe.

## What is *not* under XDG

Two categories of file deliberately live outside XDG:

- **The project working tree.** Project-local skills (`.muta/skills/`)
  and project-local commands (`.muta/commands/`) live with the project.
  They travel with the repository and are owned by the project, not the
  user's environment.
- **External applications' conventions.** muta *reads* skills from
  `~/.agents/skills/`, `~/.claude/skills/`
  because those are other tools' locations. muta never writes to them.

## Override precedence

XDG categories answer *where*. The override stack answers *who decides*.
From highest to lowest:

1. **App-specific per-category env var.** `MUTA_CONFIG_DIR`, `MUTA_DATA_DIR`,
   `MUTA_STATE_DIR`, `MUTA_CACHE_DIR`. Use these to redirect
   specific categories for muta and only muta.
2. **Instance root.** `MUTA_HOME` redirects *everything at once* —
   the four categories plus the daemon's runtime files — under one root,
   and is how development and test runs isolate themselves from an
   installed muta (ADR-0121). Sits below the per-category vars so a sandbox
   can still carve one category out.
3. **Standard XDG env var.** `XDG_CONFIG_HOME`, `XDG_DATA_HOME`,
   `XDG_STATE_HOME`, `XDG_CACHE_HOME`. Native on Linux and accepted as an
   explicit portable override elsewhere.
4. **Native per-OS default.** On macOS, `~/Library/Application
   Support/muta`; on Windows, `%APPDATA%\muta`. Provided by the
   platform's convention rather than the spec.
5. **`$HOME` fallback.** `~/.config`, `~/.local/share`, `~/.local/state`,
   `~/.cache` — the spec's default locations when nothing else applies.
6. **Current directory.** Last resort; never panics.

The same precedence applies to every category — there is no per-subsystem
special case. Relative values in the XDG env vars are ignored (per spec);
absolute values win. The daemon's runtime files follow the same idea with
their own tail: the instance root > `$XDG_RUNTIME_DIR/muta` > the data
directory, and `MUTA_PORT` plays the same role for the daemon's default
TCP port.

## What is safe to delete

| Delete | Consequence |
|--------|-------------|
| `$XDG_CACHE_HOME/muta/` | None. Cache regenerates. |
| `$XDG_STATE_HOME/muta/` | Recency-based sort orders reset; permission caches drop and re-prompt on next session. |
| `$XDG_DATA_HOME/muta/projects/<bucket>/` | That project loses its session history and embeddings. |
| `$XDG_DATA_HOME/muta/` | All history, blobs, skills, commands. Effectively a factory reset; `config.toml` survives. |
| `$XDG_CONFIG_HOME/muta/` | Loses user-edited configuration. Sessions and skills survive. |
