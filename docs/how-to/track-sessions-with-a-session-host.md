# How to track sessions with a session host

Run several neenee sessions at once — across one project or many — and keep
a live control view over all of them: which are running, which are blocked
waiting for you, which finished. neenee is built around a single user-level
**session daemon** (the daemon) that owns every session; every client (TUI,
CLI, web) talks to it over one control-plane protocol (ADR-0096).

## Concepts

- **Session daemon** (`neenee serve`, or the `neenee-server` binary): one
  process per user that hosts and manages all sessions. It starts on demand
  (the first `neenee` spawns it) or explicitly (`neenee serve`).
- **Hosted sessions**: every session is daemon-held. It keeps running when
  its TUI closes, and any client can attach to it.
- **Control plane**: the daemon's read/write API — observe (`Monitor`),
  drive (`Attach`), and manage (`CreateSession`, `SendPrompt`, `Interrupt`,
  `ResolvePermission`, `KillSession`).
- **Control view**: `neenee status` in a terminal, `/dashboard` inside a TUI,
  or `neenee dashboard` to jump straight into that full-screen view from the
  shell.

## 1. Start (or don't) the daemon

```bash
neenee serve              # foreground; prints the control-plane endpoints
neenee serve --detach     # background (auto-started on first `neenee` anyway)
neenee serve --expose     # also listen on TCP with a mandatory bearer token
```

You usually never run this yourself — any `neenee` or `neenee attach` spawns
the daemon when none is running. Run it explicitly to keep it under
systemd/tmux, or to expose the control plane to other machines.

## 2. Work as usual — everything is a client

```bash
neenee                  # attach to the daemon with a fresh/current session
neenee attach <id>      # drive a specific session
```

Because the daemon owns the session, **closing the TUI does not stop the
work**. Start a long task, close the terminal, and re-attach later:

```bash
neenee attach <id>      # the round is still running (or just finished)
```

## 3. Watch everything

Terminal, one-shot or live:

```bash
neenee status              # sessions needing attention, across all projects
neenee status --watch      # live table
neenee status --all        # also list idle sessions
neenee status --json       # raw monitor frames (scripts / a web panel)
```

Inside any TUI, press **`/dashboard`** (alias `/host`): a full-screen live
view over every daemon session — status, round/turn, output tokens, current
tool — with a detail pane for the selected row. Enter attaches to that session
**without killing the one you leave**: the TUI detaches and re-attaches, so
both sessions stay alive in the daemon. The same surface interrupts (`i`),
prompts (`p`), and creates (`n`) sessions.

Or open it straight from the shell with **`neenee dashboard`** — no need to
enter a session first. It attaches to the daemon's most-recently-active
session only as the underlying carrier and raises the dashboard over it:
**Esc quits**, **Enter** on a row attaches into that session. Like
`neenee status`, it never spawns a daemon, so it needs a running host with at
least one session.

```text
neenee dashboard — all projects — 2 session(s) needing attention
  SESSION    STATUS         ROUND      OUT ELAPSED   DETAIL
  8e439942   running        3 › 1      512 1m23s     tool bash · waiting for model · ctx 48.2k
  c71af03d   needs-approval 2          128 45s       permission: write_file
```

- **STATUS** is derived per session: `running`, `needs-approval`,
  `needs-input`, `interrupted`, `failed`, or `idle` (hidden by default).
  Blocked rows name the blocker in DETAIL.
- **ROUND `3 › 1`** = round 3, model-request 1. **OUT** = output tokens this
  round; **ELAPSED** freezes when the round ends.

## 4. Act from the control plane

The daemon is not just observability — it manages sessions. These are the
verbs the web panel and scripts use (the TUI uses attach + `/dashboard`):

| Verb | Effect |
|------|--------|
| `CreateSession { project, prompt? }` | Start a session (optionally with an opening task) |
| `SendPrompt { session_id, text }` | Queue a new round on a session |
| `Interrupt { session_id }` | Stop the current round |
| `ResolvePermission { session_id, request_id, decision }` | Approve/reject a pending tool call |
| `KillSession { session_id }` | Tear a session down |

Over the Unix socket (default) these need no token — the socket's `0600`
permissions are the boundary. Over an exposed TCP listener every call needs
`Authorization: Bearer <token>`.

## 5. Build your own panel

`neenee status --json` emits the exact frames a control panel consumes. The
full contract — handshake roles, `MonitoredSession` fields, control verbs —
is documented in [Server WebSocket API](../reference/server-api.md) and
machine-readable in [`server.asyncapi.yaml`](../reference/server.asyncapi.yaml).
A web panel is a static page that opens the monitor stream and calls control
verbs; there is no separate web backend to run.

## Scope and limits

- One daemon per user. `neenee status` aggregates every project; TUI `/dashboard`
  is the same view in-terminal.
- Sessions outlive their TUIs by design — `KillSession` (or stopping the
  daemon) is how a session ends.
- The decisions: [ADR-0096](../adr/0096-unified-session-daemon.md) (unified
  daemon + control plane), [ADR-0093](../adr/0093-daemon-observability-monitor-protocol.md)
  (monitor protocol), [ADR-0054](../adr/0054-server-layer-followups.md)
  (loopback-default security).
