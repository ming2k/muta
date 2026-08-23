# How to track sessions with a session daemon

Run several neenee sessions at once — across one project or many — and keep
a live control view over all of them: which are running, which are blocked
waiting for you, which finished. neenee is built around a single user-level
**session daemon** (the daemon) that owns every session; every client (TUI,
CLI, web) talks to it over one control-plane protocol (ADR-0096).

## Concepts

- **Session daemon** (`neenee daemon start`): one
  process per user that hosts and manages all sessions. It starts on demand
  (the first `neenee` spawns it) or explicitly (`neenee daemon start`).
- **Hosted sessions**: every session is daemon-held. It keeps running when
  its TUI closes, and any client can attach to it.
- **Control plane**: the daemon's read/write API — observe (`Monitor`),
  drive (`Attach`), and manage (`CreateSession`, `SendPrompt`, `Interrupt`,
  `ResolvePermission`, `KillSession`).
- **Control view**: `neenee daemon status` in a terminal, `/dashboard` inside a TUI,
  or `neenee dashboard` to jump straight into that full-screen view from the
  shell.

## 1. Start (or don't) the daemon

```bash
neenee daemon start       # detached by default; --fg stays in the foreground
# (detached is the default; auto-started on first `neenee` anyway)
neenee daemon start --fg --public  # all interfaces + mandatory bearer token
```

You usually never run this yourself — any `neenee` or `neenee attach` spawns
the daemon when none is running. Run it explicitly to keep it under
systemd/tmux, or to expose the control plane to other machines — see
[How to expose the daemon to LAN clients](expose-the-daemon-to-lan-clients.md).

A detached daemon runs in its own session (`setsid`), so it survives the
terminal — or the compositor hosting it — that spawned it: closing the last
terminal window does not stop the daemon or its sessions (ADR-0125). Use
`neenee daemon stop` or `kill <pid>` when you mean to stop it.

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
neenee daemon status       # sessions needing attention, across all projects
neenee daemon status --watch      # live table
neenee daemon status --all        # also list idle sessions
neenee daemon status --json       # raw monitor frames (scripts / a web panel)
```

Inside any TUI, press **`/dashboard`** (alias `/host`): a full-screen live
view over every daemon session. The surface has two zones (ADR-0097 §3): a
**console** up top — the command surface, keeping a receipt of every
directive you dispatch (what was sent, to which `#N`, and how the daemon
answered) plus the selected session's live monitor read-out (status,
round/turn, output tokens, current tool, blocking reason) — and a
**sessions dock** along the bottom, one compact card per session (sequence
number, workspace name, uptime, status). The keyboard opens on the console;
**Tab** drops to the dock. On a dock selection **Enter opens a read-only
preview** and **`a` attaches** to that session **without killing the one you
leave**: the TUI detaches and re-attaches, so both sessions stay alive in the
daemon. The same surface interrupts (`i`), suspends (`s`), kills (`k`, press
twice to confirm), prompts (`p`), and creates (`n`) sessions via the control
plane.

### The console's command line

Type anything (or press `p` for an empty line) and the footer becomes a
command line. It speaks the ADR-0097 address grammar plus slash verbs:

| Input | Effect |
|-------|--------|
| `@3 refactor the retry loop` | Send the text to session `#3` as a new round |
| `@2 @3 summarize your findings` | Fan the same prompt out to `#2` *and* `#3` |
| `fix the flaky test` | Prompt the dock selection (the classic `p` role) |
| `/interrupt` (or `/stop`) | Interrupt the selection's current round — same as `i` |
| `/interrupt @3` | Interrupt `#3` without moving the selection |
| `/suspend` (or `/park`) | Park the selection in memory: the daemon frees its RAM, and the next attach rebuilds it from disk. Refused while a client is attached or a round is active |
| `/kill` (or `/x`) | Tear the selection down. History stays on disk |
| `/new refactor the retry loop` | Create a session for this project and send the opening prompt |
| `/help` | The verb table in the console |

Every dispatch — including the `i` / `s` / `k` keys — writes a receipt line
into the console (`› [#3] prompt …` / `✓ #3 queued` / `✗ #3 session … is
not hosted on this server`), so the cockpit log answers *what did I ask the
fleet to do* at a glance.

Or open it straight from the shell with **`neenee dashboard`** — no need to
enter a session first. It attaches to the daemon's most-recently-active
session only as the underlying carrier and raises the dashboard over it:
**Esc quits**, **Ctrl+C pressed twice** does the same (the app-wide
double-press), and **`a`** on a card attaches into that session. Leaving the
screen always exits the TUI entirely — there is no conversation to fall back
into. Like
`neenee daemon status`, it never spawns a daemon, so it needs a running host with at
least one session.

```text
 DASHBOARD all projects                    2 session(s) · 1 running · 1 need attention
┌ Console ──────────────────────────────────────────────────────────┐
│ #2 fix the flaky parser test — running · round 3 › turn 1         │
│ 512 out · 1m23s · tool bash · waiting for model · ctx 48.2k       │
└───────────────────────────────────────────────────────────────────┘
┌ Sessions ─────────────────────────────────────────────────────────┐
│ #1 api-docs   45s   needs-approval   #2 parser-fix 1m23s running  │
└───────────────────────────────────────────────────────────────────┘
```

- Card **status** is derived per session: `running`, `needs-approval`,
  `needs-input`, `interrupted`, `failed`, or `idle`. Blocked sessions name
  the blocker (e.g. `permission: write_file`) in the console read-out.
- **ROUND `3 › 1`** = round 3, model-request 1. Output tokens and elapsed
  time are this-round figures; elapsed freezes when the round ends.

## 4. Act from the control plane

The daemon is not just observability — it manages sessions. These are the
verbs the web panel and scripts use (the TUI uses attach + `/dashboard`):

| Verb | Effect |
|------|--------|
| `CreateSession { project, prompt? }` | Start a session (optionally with an opening task) |
| `SendPrompt { session_id, text }` | Queue a new round on a session |
| `Interrupt { session_id }` | Stop the current round |
| `ResolvePermission { session_id, request_id, decision }` | Approve/reject a pending tool call |
| `SuspendSession { session_id }` | Park a session in memory only — the daemon frees its RAM, `SessionEnd` hooks do not fire, and the next attach rebuilds it from disk (lazy resume). Refused while a client is attached or a round is active |
| `KillSession { session_id }` | Tear a session down |
| `Shutdown` | Stop the daemon itself — the same graceful drain as Ctrl-C/SIGTERM (what `neenee daemon stop` sends) |

Over native local IPC these need no token: Unix uses a `0600` socket and
Windows uses a Named Pipe protected for the current user. Over an exposed TCP
listener every call needs `Authorization: Bearer <token>`.

## 5. Stop the daemon

```bash
neenee daemon stop       # graceful, through the control plane
kill <pid>               # SIGTERM runs the same drain (pid is in `neenee daemon status`)
```

On Windows, use `neenee daemon stop`; the protocol drain is the portable
shutdown contract. Process termination is only the final force tier.

Both run the same budgeted drain: stop accepting, close live connections
(watch clients get a `daemon_draining` frame first), fire every session's
`SessionEnd` hooks — each under its own deadline — remove the discovery
record, and exit 0 within the grace budget (`[daemon] shutdown_grace_secs`,
default 10s; a second signal skips the wait). Left alone, the daemon also
exits by itself after `[daemon] idle_exit_minutes` (default 5) with nothing
hosted and nobody attached; pass `--idle-exit 0` (or set the config key) for
an always-on deployment — see
[`assets/neenee.service`](https://github.com/ming2k/neenee/blob/main/assets/neenee.service)
for a ready systemd user unit.

After a restart, the daemon brings autonomous sessions back on its own
(ADR-0125): any persisted session with armed `/schedule` jobs is rehosted at
boot — scheduled prompts keep firing across daemon restarts, crashes, and
reboots without anyone attaching first. Opt out with
`[daemon] rehost_armed_schedules = false` (sessions then stay dormant until
attached, the pre-0125 behavior).

## 6. Build your own panel

`neenee daemon status --json` emits the exact frames a control panel consumes. The
full contract — handshake roles, `MonitoredSession` fields, control verbs —
is documented in [Server WebSocket API](../reference/server-api.md) and
machine-readable in [`server.asyncapi.yaml`](../reference/server.asyncapi.yaml).
A web panel is a static page that opens the monitor stream and calls control
verbs; there is no separate web backend to run.

## Scope and limits

- One daemon per user. `neenee daemon status` aggregates every project; TUI `/dashboard`
  is the same view in-terminal.
- Sessions outlive their TUIs by design — `KillSession` (or stopping the
  daemon) is how a session ends.
- The decisions: [ADR-0096](../adr/0096-unified-session-daemon.md) (unified
  daemon + control plane), [ADR-0093](../adr/0093-daemon-observability-monitor-protocol.md)
  (monitor protocol), [ADR-0054](../adr/0054-server-layer-followups.md)
  (loopback-default security), [ADR-0101](../adr/0101-daemon-shutdown-correctness.md)
  (shutdown correctness: budgeted drain, signals, `neenee daemon stop`).
