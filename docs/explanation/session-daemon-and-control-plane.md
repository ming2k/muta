# The session daemon and the control plane

How neenee's processes are arranged, who owns a session's lifecycle, and how
every client — the TUI, the CLI, a web panel — talks to the same place. This
is the conceptual companion to [ADR-0096](../adr/0096-unified-session-daemon.md);
for the wire contract see the [Server WebSocket API](../reference/server-api.md),
and for day-to-day use see
[How to track sessions with a session daemon](../how-to/track-sessions-with-a-session-daemon.md).

## The shape

```
┌─ neenee (the CLI) ──────────────── every verb is a client call
│    serve / attach / status
│
├─ neenee-server (the daemon) ────── one process per user; owns every session
│    session plane:  a SessionRegistry hosting N sessions across N projects
│    control plane:  observe (Monitor) · drive (Attach) · manage (Control)
│      ├─ Unix socket  (default; file permissions are the auth boundary)
│      └─ TCP + token  (--public; for LAN clients and the web panel)
│
└─ clients ───────────────────────── all speak the control plane
     TUI (/host, attach)   neenee status   a web control panel   scripts
```

One user-level daemon — not one per project, not one per session — holds
every session the user is running. The CLI is the core surface; the TUI is
the closest client; anything with a better UI (a web panel) is a consumer of
the same API, not a new backend.

## Why a single owner

The architecture answers one question: **who does a session belong to?**

Before ADR-0096 the answer was "whoever started it" — a TUI process held its
own session, so closing the terminal ended the work, and a control view could
only describe some sessions. The unified answer is **the daemon**:

- A session **outlives any client**. Closing a TUI detaches; the round keeps
  running; re-attach from anywhere.
- **Switching never kills work.** Moving between sessions is detach + attach
  against the daemon, not a cancel — the round you leave keeps running.
- **One place to see and manage everything.** Observability (ADR-0093's
  monitor stream) and management (the control verbs) live behind one
  handshake, so a panel never has to discover and merge N processes.

This is the tmux/docker trade, adopted deliberately: thin clients,
centralized state. The cost — a background daemon always runs while any
session exists, and first launch pays one cold start — is recorded in
ADR-0096's consequences rather than hidden.

## The control plane

One WebSocket handshake (`Select`) chooses a role; everything shares the
socket:

- **Attach** — bidirectional: drive a session (`Request`/`Response`), as a
  TUI does.
- **Monitor** — read-only stream: a snapshot then whole-row diffs, one
  `MonitoredSession` per session (status, round/turn, tokens, current tool,
  blocking reason). No conversation content, so a dashboard never parses a
  transcript.
- **Control** — one verb per connection: `create_session`, `send_prompt`,
  `interrupt`, `resolve_permission`, `kill_session`. This is what turns
  "watching" into "managing".

Two transports carry it:

- **Unix domain socket** (default, `$XDG_RUNTIME_DIR/neenee/daemon.sock`) —
  the local channel for the CLI and TUI. `0600` in a `0700` runtime dir;
  the filesystem *is* the authentication, so no token.
- **TCP + bearer token** (`neenee serve --public`; `--expose` on the
  `neenee-server` binary) — for LAN clients and the web panel. Exposing is
  always an explicit opt-in that carries a token (ADR-0054's model); TLS is
  fronted by a reverse proxy. See
  [How to expose the daemon to LAN clients](../how-to/expose-the-daemon-to-lan-clients.md).

A web control panel is therefore a static page that opens the monitor stream
and calls control verbs — no web-specific server exists or is needed.

## Lifecycle in one pass

1. Any `neenee` or `neenee attach` finds no live daemon record
   (`daemon.json`) and spawns `neenee-server` detached; or you run
   `neenee serve [--detach]` yourself.
2. The daemon binds the UDS (always) and a TCP port (loopback by default,
   exposed with `--public`), writes the global discovery record, and waits.
3. Sessions are created on demand (a client's attach, or a control
   `create_session`) and assembled lazily from disk on first attach.
4. Clients observe via `status` / `/host`, drive via attach, manage via the
   control verbs. Closing a client never disturbs the session.
5. On shutdown the daemon cancels its sessions' drivers and removes the
   discovery record and socket.

## What this replaced

- The **per-project, one-session-per-process** host (ADR-0081) — generalized
  by ADR-0089's registry and superseded by ADR-0096's single daemon.
- **Session mirroring** (ADR-0095) — a bridge that let standalone TUIs report
  into the panel without changing ownership. With unified ownership there is
  nothing to mirror, so the bridge is removed.

## References

- [ADR-0096](../adr/0096-unified-session-daemon.md) — the decision and its
  consequences (including the behaviour changes).
- [ADR-0093](../adr/0093-daemon-observability-monitor-protocol.md) — the
  monitor protocol the control plane extends.
- [ADR-0094](../adr/0094-serve-as-host-verb.md) — the serve/attach/status
  verb vocabulary.
- [ADR-0054](../adr/0054-server-layer-followups.md) — the loopback-default
  security model.
- [Server WebSocket API](../reference/server-api.md) — the wire contract.
