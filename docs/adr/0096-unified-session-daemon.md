# 0096. Unified session daemon and the control plane

- **Status:** Accepted
- **Date:** 2026-08-08
- **Builds on:** ADR-0089 (multi-session host), ADR-0093 (monitor protocol),
  ADR-0094 (serve vocabulary), ADR-0095 (mirroring)
- **Supersedes:** ADR-0095 (the ownership-preserving mirror — unified
  ownership makes it obsolete)

## Context

ADR-0089/0093/0095 built a per-project session host with an observability
surface, and ADR-0095 closed the last visibility gap by letting standalone
TUIs mirror into it. But the result is still a constellation of partial
answers:

- One host **per project**: no way to see or act on every session a user is
  running across projects — the "one control panel" the whole line of work
  exists for.
- A **read-only** surface: you can watch a session starve on a permission
  prompt but cannot approve it, interrupt it, or start a new one without
  going back to a TUI.
- **Two session models**: hosted sessions (host-owned, re-attachable,
  survivable) and mirrored sessions (TUI-owned, observability-only,
  non-attachable). ADR-0095 itself flagged the unification decision as
  deferred. The split is the root of every "why does switching kill my
  round" / "why can't I drive this row" rough edge.

The architecture this ADR adopts is the one the codebase has been converging
on: **one user-level daemon owns every session; every client — TUI, CLI,
web — talks to it over one control-plane protocol.**

## Decision

### 1. One user-level daemon

the `neenee-server` binary (the unified session daemon; `neenee serve` runs it in the
foreground) holds **all** sessions across **all** projects for the user.
The registry indexes sessions as `project_root → session_id`; the monitor
snapshot can be filtered by project. The discovery record is global —
`$XDG_RUNTIME_DIR/neenee/daemon.json` — replacing the per-project bucket
files. Legacy per-project records are ignored (harmless litter); the daemon
removes its record on clean shutdown as before.

- **Passive start**: any `neenee` / `neenee attach` spawns the daemon when
  no live record exists (the existing `ensure_server`, promoted to global).
- **Active start without a session**: `neenee serve` (foreground) or
  `neenee serve --detach` (new: fork to the background, write a pidfile).
  Starting the daemon creates no session.

### 2. The daemon owns every session (supersedes ADR-0095)

`neenee` with no subcommand is now equivalent to `neenee attach`: it
connects to the daemon (spawning it if needed) and drives a daemon-held
session. There is no in-process harness path and no mirroring — the ADR-0095
mirror channel, tap, and `SessionHosting` distinction are removed. Every
session is `hosted`: re-attachable, survivable across TUI exits, visible
and drivable from every client.

Consequences of this ownership change, stated plainly:

- **TUI exit no longer ends the round.** The session lives in the daemon;
  closing the terminal detaches. Re-attach to the same session any time.
- **First start pays daemon cold-start once** (config discovery, MCP
  connects, skill scans); every later attach is a connection, not an
  assembly.
- **Session switching never kills work.** Entering another session is
  detach + attach; the previous round keeps running in the daemon. This
  replaces ADR-0089-era `/session open`'s `supersede_for_session_switch`
  behaviour for the attach path.

### 3. A control plane, two transports

The socket protocol grows from observability into session **management**,
still JSON-over-WebSocket, still one handshake (`Select`):

```
Attach    → Welcome + bidirectional Request/Response     (as before)
Monitor   → snapshot + diffs, optional project filter    (ADR-0093, extended)
Control   → CreateSession { project, prompt? }           (new)
            Interrupt { session_id }                     (new)
            ResolvePermission { session_id, request_id, decision } (new)
            KillSession { session_id }                   (new)
```

Two transports serve it:

- **Unix domain socket** (default): `$XDG_RUNTIME_DIR/neenee/daemon.sock`,
  filesystem permissions as the auth boundary. CLI and TUI use this — zero
  configuration, nothing network-visible.
- **TCP + bearer token** (`--expose [addr]`): for LAN clients and the web
  panel. Same security model as ADR-0054: exposing is an explicit opt-in
  that always carries a token; TLS is fronted by a reverse proxy.

The web control panel is a static page that consumes `Monitor` and invokes
`Control` verbs — an API client, exactly like the TUI; no web-specific
backend is added.

### 4. CLI is the core; TUI the closest client

- `neenee serve [--expose [addr]] [--detach]` — the daemon verb.
- `neenee attach [id]`, `neenee status [--watch/--json/--all]`,
  `neenee dashboard` — clients of the control plane.
  `status` shows every project; `--project <path>` filters. (A `neenee
  sessions` listing verb was sketched here but not implemented; the
  dashboard and `status` cover the listing role.)
- **`/host` in the TUI** — the in-terminal control panel (ADR-0093's
  deferred follow-up, now enabled): a live view over the daemon's sessions
  with per-row status/preview; Enter attaches to the selected session
  (detach + attach, never killing its round). The `neenee attach` Pick
  prompt (ADR-0089's deferred follow-up) is folded into the same view.
- **`neenee dashboard`** — that same in-terminal control panel reachable
  straight from the shell, without first entering a session. The dashboard's
  monitor stream and its control verbs ride their own daemon connections, so
  it never depends on the attached session; the client attaches to the
  most-recently-active hosted session purely as the underlying TUI carrier
  and raises the dashboard over it. Esc from that opening dashboard quits
  (there is no conversation the user asked for behind it); Enter on a row
  attaches as usual. Like `status` it never spawns a daemon — a missing
  daemon or an empty host is a clean error, not an excuse to spawn one or
  fabricate a session. (`/host` was the pre-dashboard name for the panel;
  it survives as a hidden alias.)

## Alternatives considered

- **Keep per-project hosts.** Rejected: the control panel's entire purpose
  is a cross-project view; per-project hosts force every client to
  enumerate and merge N daemons, and make "manage every session" a
  client-side problem forever.
- **Keep mirroring; unify only the panel.** Rejected: two session models
  permanently split what "enter this session" means. ADR-0095 was a
  deliberate bridge; the destination is single ownership.
- **HTTP/REST control API.** Rejected (again, now definitively): the
  transport already streams over WebSocket; a second protocol family would
  split auth, versioning, and client code. The control plane extends the
  existing handshake.
- **gRPC/cap'n proto.** Rejected: adds an IDL toolchain and binary framing
  to a surface browsers must consume natively; JSON-over-WS is the
  lowest-common-denominator that every client (TUI, web, scripts) already
  speaks.

## Consequences

- **Positive.** One place owns all state: one panel sees everything, one
  API manages everything, one process to supervise.
- **Positive.** The mirrored/hosted split, the `/session open` round-kill,
  and the attach-Pick stderr hack all disappear — three long-standing rough
  edges share one root cause and one fix.
- **Positive.** Web and LAN clients become possible with zero new protocol
  work beyond the control verbs.
- **Negative (accepted).** The zero-background single-process `neenee` is
  gone. A daemon always runs while any session exists. This is the
  tmux/docker trade: thin clients, centralized state. It is recorded here
  as a deliberate product position, not a side effect.
- **Negative (bounded).** Sessions now outlive their TUIs; a forgotten
  session keeps its (idle) daemon-side cost until killed or the daemon
  stops. `KillSession` and idle reaping (a follow-up) bound this.
- **Breaking.** The discovery record moves; `SessionHosting`/the mirror
  channel are removed; `neenee` with no subcommand changes behaviour
  (daemon-held session). All on the still-unreleased serve/attach surface.
- **Follow-ups.** Web panel; idle-session reaping; `KillSession` wiring in
  the TUI panel; TLS for the exposed listener without a proxy.

## References

- [ADR-0089](0089-multi-session-daemon.md) — the per-project host this
  generalizes.
- [ADR-0093](0093-daemon-observability-monitor-protocol.md) — the monitor
  protocol the control plane extends.
- [ADR-0094](0094-serve-as-host-verb.md) — the verb vocabulary; "daemon" is
  now the deployment reality this ADR makes true.
- [ADR-0095](0095-standalone-session-mirroring.md) — superseded; its §
  "follow-up: evaluate full unification" is this ADR.
