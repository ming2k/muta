# ADR-0112: Client-Declared Session End

| Status   | Accepted |
|----------|----------|
| Date     | 2026-02-14 |
| Supersedes | — |

## Context

ADR-0096 gave the daemon a lifetime anchored to hosted sessions: a session
stays hosted after its clients detach, so `neenee attach` can pick it back
up. That is the right default — but it left a gap. When an operator is
*done* with a session, there was no way to say so:

- The TUI's `/exit` quit the frontend and dropped the socket. The session
  stayed hosted forever. The idle reaper (ADR-0096) could never reclaim it:
  it only applies to never-persisted sessions with no content, and a
  session the operator just used has both.
- The web panel's only session-management actions were rename and *delete*
  (which erases disk history) — nothing between "leave it hosted
  indefinitely" and "destroy it".
- Dashboards accumulated dead rows: a session whose client is gone and
  whose operator will never return still shows as hosted, `idle`, forever.

Closing a window / killing a client process cannot mean "end the session" —
that is indistinguishable from a crash or a network drop, and detaching
(on-purpose, session keeps running in the background) is a first-class flow
(ADR-0096 `/host` panel). Only the *client* knows the difference.

## Options considered

1. **Bind session lifetime to connection/reference counting** (last client
   disconnect ⇒ end). Rejected: it erases the detach-keeps-running flow
   and conflates crashes with intent. A reference count also cannot see
   half-open TCP connections without a keepalive layer this daemon does
   not have (no WS ping/pong exists in the codebase).
2. **Extend the idle reaper with a last-active TTL.** Rejected as the
   primary mechanism: "the client left" and "the session is stale" are
   different facts; a TTL delays the dashboard cleanup by design and ends
   sessions nobody asked to end (a long-running background round that is
   merely quiet).
3. **An explicit client-declared end.** The client states intent; the
   daemon tears the session down deterministically. Chosen.

## Decision

Add a wire message by which a client declares the session over. Three
surfaces converge on one teardown path:

1. **`AgentRequest::EndSession`** (unit variant, in-band on the existing
   attach connection). The server **intercepts it at the connection layer**
   (`serve.rs`) — it never enters the driver queue, because the driver is
   exactly what the teardown is about to cancel; queueing would race the
   cancellation.
2. **`ControlRequest::KillSession`** (already existed, ADR-0096) for
   session management without an attach connection. The web panel's new
   "end session" action uses this verb.
3. Both funnel into the same `SessionRegistry::kill_session` pipeline that
   daemon shutdown uses: remove from the registry, cancel the driver
   (which reaps MCP/bash child processes via `kill_on_drop`), broadcast
   the terminal `AgentResponse::Exit` to attached clients, fire bounded
   SessionEnd hooks (ADR-0025), clear WIP declarations (ADR-0097), and
   publish `MonitorEvent::SessionRemoved` so every dashboard — TUI
   `/dashboard`, web panel, `neenee status --watch` — drops the row
   immediately.

### Who sends it

| Surface | Trigger | Note |
|---|---|---|
| TUI | `/exit` | locally intercepted in the composer, now also sends `EndSession` before exiting |
| TUI | double `Ctrl+C` (armed quit window) | same "done with this session" semantics |
| headless CLI | terminal round (completed or errored) | waits (bounded, 3s) for the daemon's `Exit` ack so the teardown is deterministic |
| web panel | "end session" action (⏻ in the sidebar) | via the `kill_session` control verb |

Detach-flavoured exits do **not** send it: the `/host` switch (quit +
re-attach to another session), the startup picker/dashboard overlays'
Esc/Ctrl+C cancel (no session was chosen; the carrier session stays
hosted), and plain socket drops (crash, network) — those keep the
session hosted, exactly as ADR-0096 intended.

### What "ended" means

- **In-memory hosting is gone**: registry entry, driver task, broadcast
  bus, tracker, WIP declarations.
- **Disk history is kept**: `<sessions_dir>/<id>.json` + `.jsonl` remain;
  `/sessions` can still resume the transcript. Ending a session is not
  deleting it (`DeleteSession` remains the verb that erases history).
- **A running round is cancelled immediately** (chosen over waiting for
  the round to finish): the operator's exit is an instruction, not a
  request to keep spending tokens; cancellation reuses the same unwind
  path as `Interrupt`.

### Reliability

The in-band `EndSession` is fire-and-forget from the TUI (its process is
about to exit). Two details make that safe:

- The client's request pump drains its channel to the wire *before*
  closing the socket (`client.rs`), and after flushing an `EndSession` it
  pauses briefly (150ms) so the runtime does not rip the connection away
  mid-teardown.
- Over UDS/TCP the written bytes are kernel-buffered, so even a client
  that dies instantly still delivers the frame.

Headless, which stays alive, waits for the `Exit` ack instead (bounded).

## Consequences

- Dashboards reflect session reality again: rows disappear the moment the
  owning client says goodbye, not "never".
- The daemon's idle-exit (ADR-0100) responds faster: ending the last
  session lets the host exit without waiting for the idle-empty grace.
- `neenee attach <id>` against an ended session fails ("not hosted") —
  that is correct; resume-the-transcript is the recovery path.
- The daemon still cannot distinguish a crashed client from a silently
  detached one. That ambiguity is deliberately left to a future keepalive
  / last-active ADR (see "Alternatives" above for why connection-lifetime
  binding alone would be wrong).
- `/exit` now ends the hosted session, which changes ADR-0096's
  "`/exit` leaves the session hosted" behavior. This is the fix the gap
  motivated: `/exit` is the operator saying "done".
