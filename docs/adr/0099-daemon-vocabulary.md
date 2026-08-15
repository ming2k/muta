# 0099. Daemon vocabulary: bless "daemon", keep artifact names

- **Status:** Accepted
- **Date:** 2026-08-14
- **Revises:** ADR-0094 (revokes the "daemon is reserved for OS deployment
  style" clause; its serve/host verb-and-concept decisions stand)

## Context

One process answers to three names across the repository: the binary is
`neenee-server`, the code namespace says *host* (`neenee-host`, `host.rs`,
`HostOptions`), and prose says *daemon* (`daemon.json`, the ADR-0096 title,
~33 occurrences across eight docs, the glossary's existing **session daemon**
entry).

ADR-0094 tried to prevent exactly this by reserving the word "daemon" for
OS-managed deployment styles (systemd/launchd) and picking `serve`/`host` as
the sanctioned verb and concept. The reservation failed in practice: the word
"daemon" kept winning because it is the correct, universally understood term
for a per-user, on-demand background process — gpg-agent and ssh-agent are
daemons without any service manager, and the tmux server is one in
everything but name. A vocabulary rule that fights the language leaks
continuously; the visible symptom was a how-to guide titled "track sessions
with a session host" whose body immediately gives up and says "session
daemon".

Meanwhile the *artifact* names are fine: `neenee-server` pairs with the
`serve` verb, and `neenee-host` accurately names the library crate.

## Decision

Adopt a four-slot vocabulary and bless it as canonical:

- **The process** is the **(session) daemon** — prose, user docs, and the
  `daemon.json` discovery record already use this; no reservation on the
  word. "Daemon" covers both on-demand-spawned and service-manager-run
  deployments.
- **The code namespace** stays **host** — the `neenee-host` crate, `host`
  module, `HostIdentity`/`HostOptions` are unchanged.
- **The verb** stays **serve** — `neenee serve` runs the daemon in the
  foreground; `--detach` backgrounds it.
- **The binary** stays **`neenee-server`** — unchanged; it pairs with the
  verb and the client/server architecture.

No artifact is renamed. The glossary maps the slots; prose uses "the daemon"
/ "the session daemon" for the process; "the server" remains correct only in
the networking sense (the WebSocket server inside `serve.rs`, MCP servers,
provider servers).

## Alternatives considered

- **Enforce ADR-0094's reservation and purge "daemon" from prose.**
  Rejected: it fights the language. Every reader and several maintainers
  already call the process a daemon; enforcing "host" in prose would be
  permanent vigilance for zero information gain.
- **Rename the binary to `neenee-daemon`.** Rejected: artifact renames are
  for *wrong* names (ADR-0098's `core`/`transport`), not for choosing
  between two acceptable synonyms. `neenee-server` pairs with `serve`, and
  the rename would churn install scripts, process lists, and user muscle
  memory.
- **Status quo.** Rejected: three unmapped names for one process is a real
  reading tax, and the failure of the reservation clause proves it does not
  self-correct.

## Consequences

- ADR-0094's daemon-reservation clause is revoked; its other decisions
  (serve as the verb, host as the concept in code) stand.
- The how-to guide is renamed to
  `track-sessions-with-a-session-daemon.md`; link sites updated.
- `docs/reference/glossary.md` cross-references this ADR from the
  **session daemon** entry.
- No code, config, wire-protocol, or file-name changes (`daemon.json`
  already matches the blessed word).

## References

- [ADR-0094](0094-serve-as-host-verb.md) — the verb vocabulary; reservation
  clause revoked here.
- [ADR-0096](0096-unified-session-daemon.md) — the unified session daemon.
- [Glossary](../reference/glossary.md) — the living vocabulary surface.
