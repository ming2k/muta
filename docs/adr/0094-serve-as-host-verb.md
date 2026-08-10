# 0094. Serve as the host verb: unify the session-host vocabulary

- **Status:** Accepted
- **Date:** 2026-08-08
- **Revises:** ADR-0089 §4 (CLI surface) — `neenee daemon` is renamed before
  ever shipping
- **Builds on:** ADR-0089, ADR-0093

## Context

ADR-0089/0093 shipped the multi-session host with three different names for
the same role scattered across the surface: the user-facing subcommand was
`neenee daemon`, the binary was `neenee-server`, and the internal modules
spoke of `serve` (`serve.rs`, `serve_discovery.rs`, the `/serve` TUI
command, the `serve/` discovery directory). Two of the three were
problematic on their own merits:

- **`daemon` lied twice.** It names a *deployment style* (detached
  background process) rather than the role — and the subcommand actually ran
  in the **foreground**, the opposite of what `emacs --daemon` /
  `git daemon --detach` users are trained to expect. ADR-0089 explicitly
  scoped out service management, so nothing about it was a daemon except the
  label.
- **The internal vocabulary had already converged on "host"** —
  `HostedSession`, `HostParams`, `SessionRegistry::host()`,
  `prehost_only()`, "this host cannot create sessions" — while the file was
  called `daemon.rs` and its types `DaemonIdentity`/`DaemonOptions`.

ADR-0089 noted the whole attach surface was unreleased and breaking changes
were acceptable; ADR-0093 added `status` to the same surface. The rename
window closes the moment any of this ships.

## Decision

One role, one word per layer:

| Layer | Word | Form |
|---|---|---|
| User verb (foreground host) | **serve** | `neenee serve [--port <n>] [--public]` |
| Client verb | **attach** | `neenee attach [id]` (unchanged) |
| Observation | **status** | `neenee status [--watch/--json/--all]` (unchanged) |
| Process/binary | **server** | `neenee-server` (unchanged; tmux precedent) |
| Transport modules | **serve** | `serve.rs`, `serve_discovery.rs`, `/serve`, `serve/` discovery dir (unchanged) |
| Host runtime module | **host** | `host.rs`; `HostIdentity`, `HostOptions` (was `daemon.rs`, `Daemon*`) |
| Deployment style | **daemon** | prose only — "run `neenee serve` as a systemd daemon"; never a command or type name. ADR-0096 later makes the daemon real (`serve --detach`); the verb stays `serve` |

Concretely:

1. `StartupMode::Daemon` → `StartupMode::Serve { port, public }`; `neenee
   serve` parses `--port`/`--public` with the same semantics as the
   `neenee-server` binary (the subcommand is the verb, the binary is the
   implementation — ADR-0089's own framing).
2. `neenee_transport::daemon` → `neenee_transport::host`; `DaemonIdentity` /
   `DaemonOptions` → `HostIdentity` / `HostOptions`, converging with the
   existing `HostParams` vocabulary.
3. `neenee serve` (foreground, typed interactively) prints its bind address,
   port, and how to observe/drive it to stderr on startup — a foreground
   command must be self-describing.
4. No alias, no compat shim: the `daemon` subcommand was accepted in
   ADR-0089 but **never released** (it did not even parse until ADR-0093
   fixed the parser), so there is nothing to migrate. Follows the ADR-0042
   precedent (hard rename, no alias).
5. Error/usage strings across `neenee status`, `neenee attach`, and
   `neenee-server` adopt "session host" / "serve" wording.

## Alternatives considered

- **Keep `daemon` as the verb.** Rejected: it mis-describes a foreground
  command, and every doc sentence would need a footnote ("daemon, but
  foreground, and not managed"). A name that needs a disclaimer on every use
  is a bad name.
- **`host` as the CLI verb.** Rejected: `host` reads as a noun (`neenee
  host` parses as "the host of neenee"), and it would collide with the
  internal type vocabulary the CLI layer already uses. It stays the internal
  noun; `serve` stays the verb — the same noun/verb split as
  attach/`AttachAction`.
- **Keep `daemon.rs` and only rename the subcommand.** Rejected: that keeps
  the exact split-brain this ADR exists to remove (file says daemon, types
  say host, command says serve).
- **`neenee up` / `neenee start`.** Rejected: `up` implies lifecycle
  management (down/restart) we do not have; `start` implies a stopped state
  the user manages. `serve` states exactly what the process does while it
  runs.

## Consequences

- **Positive.** Every surface names the role it plays; the grep chain
  `serve` (module) → `serve` (discovery) → `serve` (subcommand) →
  `server` (binary) now answers "where is this implemented?" without a
  translation table.
- **Positive.** `neenee serve --port 8765 --public` gives the subcommand
  parity with the binary, so users no longer need to know the binary exists
  to run a public listener.
- **Neutral.** "Daemon" remains available as the correct word for a *future*
  deployment feature (`neenee serve --detach`, systemd units) — it will then
  describe a real background mode instead of borrowing the term.
- **Breaking (unreleased surface).** `neenee daemon` disappears before its
  first release; `DaemonIdentity`/`DaemonOptions`/`daemon::run` are renamed.
  Recorded in the changelog.

## References

- [ADR-0089](0089-multi-session-daemon.md) — the host architecture; §4's CLI
  surface is revised here.
- [ADR-0093](0093-daemon-observability-monitor-protocol.md) — the monitor
  protocol; its prose use of "daemon" is descriptive, not prescriptive.
- ADR-0042 — the hard-rename precedent.
- [ADR-0054](0054-server-layer-followups.md) — secure serve defaults
  (`--public` ⇒ bearer token), unchanged.
