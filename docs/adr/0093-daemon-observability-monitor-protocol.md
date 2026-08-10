# 0093. Daemon observability: the monitor protocol and `neenee status`

- **Status:** Accepted
- **Date:** 2026-08-08
- **Builds on:** ADR-0089 (multi-session daemon), ADR-0081 (server + attach
  model), ADR-0017 (per-session event envelopes)

## Context

ADR-0089 gave the daemon a registry that hosts N sessions, and every hosted
session already broadcasts its full `AgentResponse` stream to any number of
attached clients. But the only way to *observe* the daemon is to attach as a
full session client: the `Welcome` handshake ships the entire transcript, and
the client is then bound to one session. A control panel — the promised
"central view of every task, done or not" — cannot be built on that contract:

- there is no way to enumerate hosted sessions without driving one;
- there is no session-lifecycle vocabulary above the round stream ("which
  sessions are running / blocked / failed?"), so each observer would have to
  re-derive it from raw events;
- attaching pulls megabytes of transcript a dashboard never renders.

Meanwhile two rough edges had accreted: `neenee daemon` was accepted by
ADR-0089 and handled in `main.rs`, but `parse_args` never actually parsed the
subcommand (the branch was unreachable), and the daemon/attach surface was
absent from the READMEs entirely.

## Decision

### 1. A read-only observability channel, not a control plane

Monitoring is a **one-way, server → client** stream. A monitor client can
never steer a session: after the handshake the server ignores every inbound
frame except close. This keeps the ADR-0018 one-writer invariant trivially
intact and makes the channel safe to expose to any number of panels.

### 2. `Select{action: Monitor(MonitorAction)}` joins the handshake

The select-then-attach protocol (ADR-0089 §2) gains one attach action and one
frame kind:

```rust
enum AttachAction { New, Attach(Option<String>), Monitor(MonitorAction) }

struct MonitorAction { watch: bool, include_idle: bool }  // both default false

enum Wire {
    // …existing variants…
    Monitor { event: MonitorEvent },   // server -> client only
}

enum MonitorEvent {          // serde tag = "kind"
    Snapshot(MonitorSnapshot),          // always the first frame
    SessionAdded(MonitoredSession),     // whole row, no back-reference needed
    SessionUpdated(MonitoredSession),   // idempotent whole-row replacement
    SessionRemoved { session_id },      // reserved: teardown is not yet emitted
}
```

Semantics:

- The server always sends `Snapshot` first. With `watch: false` it then
  closes the connection (one-shot poll, `neenee status`). With `watch: true`
  it streams diffs until the client hangs up.
- `include_idle: false` (the default) filters the snapshot and the diff
  stream to sessions that are **not** `Idle`, so a busy dashboard stays a
  zero-statement surface: an all-quiet daemon reports an empty list.
- The client subscribes to the registry's daemon-level
  `broadcast<MonitorEvent>` **before** the snapshot is composed, so an event
  racing the snapshot arrives as a redundant diff — safe because updates are
  idempotent whole-row replacements. A lagging watcher is resynced with a
  fresh `Snapshot`, the same recovery the session broadcast uses.
- Diffs carry whole rows, not field patches. Rows are small (no content) and
  consumers stay trivial: `upsert by id`.

### 3. `SessionStatus`: derived display state, not protocol state

Each hosted session gets a `MonitorTracker` (in `neenee_transport::monitor`)
that folds its broadcast stream into one row:

```rust
enum SessionStatus { Idle, Running, NeedsApproval, NeedsInput, Interrupted, Failed }

struct MonitoredSession {
    id, overview, created_at, updated_at, message_count,   // cheap header
    status, round, turn, output_tokens, elapsed_ms,        // lifecycle + accounting
    current_tool, activity, context_tokens, note,          // at-a-glance detail
}
```

The derivation mirrors the single-session `ParentStatus` badge (ADR-0017) and
respects ADR-0078: the round lifecycle itself stays binary, and
`NeedsApproval` / `NeedsInput` are overlays on a still-running round, cleared
when model output resumes (`StreamStart`/`StreamDelta`/`StreamEnd`).
`RoundCompleted` → `Idle`, `Error` → `Failed`, `UnsentInput` → `Interrupted`;
`elapsed_ms` runs from the round's first `TurnStarted` and freezes at the
terminal event. The tracker is the *only* place this folding exists, so every
frontend (today's table, tomorrow's web panel) reads one vocabulary.

Monitor rows carry **no conversation content** — ids, title/preview, status,
and accounting only — so a panel never deserializes a transcript.

### 4. One daemon-level topic in the registry

`SessionRegistry` owns a `broadcast::Sender<MonitorEvent>` (capacity 256).
The per-session broadcast-tap task — which already forwards driver responses
onto the session topic — now also folds each response into the session's
tracker and publishes `SessionUpdated`. Session creation publishes
`SessionAdded`. The registry exposes `subscribe_monitor()` +
`monitor_snapshot(action)`; `serve::run_monitor` is the only consumer-facing
assembly of the two. `daemon::run` stamps the registry with its project root
and start time so snapshots identify the host; a `/serve` prehost reports
`started_at: 0`.

### 5. CLI surface: `neenee status [--watch] [--json] [--all]`

- `neenee status` — one snapshot, human table, exit. Lists only sessions
  needing attention (running / blocked / failed / interrupted); `--all` adds
  idle sessions.
- `--watch` — keep the stream open and redraw the table on every diff.
- `--json` — emit the `Wire::Monitor` frames as JSON lines (the exact
  contract a web control panel will consume).

Unlike `neenee attach`, `status` **never spawns a daemon**: observing is only
meaningful against a running host, so a missing or stale discovery record is
a clean "no daemon is running" error, not an excuse to start one.

### 6. Fix the `neenee daemon` parse gap

`parse_args` gains the `daemon` arm ADR-0089 promised (making the existing
`main.rs` branch reachable) and the usage text finally lists `daemon`,
`attach`, and `status`.

## Alternatives considered

- **Poll `Pick` repeatedly.** `Pick` exists for the attach handshake and
  carries only picker rows — no status, no accounting, no streaming. A panel
  polling it would still be blind to *why* a session is busy. Rejected.
- **Attach to every session and derive status client-side.** Multiplies
  transcript downloads by N sessions and re-implements the folding in every
  observer. Rejected; the tracker derivation is now exactly one place.
- **A separate HTTP/JSON control endpoint.** A second listener, a second auth
  story, and a second protocol to version — when the existing WS stack
  already does handshake, auth, and streaming. Rejected; the monitor channel
  is one more `Select` action on the socket the daemon already serves.
- **Field-level diffs (`SessionPatched { changed_fields: … }`).** Saves bytes
  but forces every consumer to implement merge logic and makes lag recovery
  lossy. Whole-row replacements are idempotent and keep panels stateless-
  simple. Rejected.

## Consequences

- **Positive.** A control panel needs exactly one connection, one snapshot,
  and an upsert loop — the JSON-over-WS contract is directly consumable from
  a browser with no backend in between.
- **Positive.** `neenee status` gives multi-task tracking today: blocked
  sessions surface as `needs-approval` / `needs-input` with the blocking
  prompt as the note, which is the "which task needs me" question the panel
  exists to answer.
- **Positive.** `neenee daemon` actually works now, and the usage text
  advertises the whole daemon surface.
- **Neutral.** `SessionRemoved` is reserved but unemitted — hosted sessions
  live for the daemon's lifetime today. Panels written against the contract
  handle teardown when it lands.
- **Neutral.** The per-event `SessionUpdated` publish is one small clone per
  broadcast response; negligible next to the response itself, and the
  publish is a no-op when no monitor is subscribed.
- **Follow-ups (not in this ADR).** A cross-project/global registry (one
  panel over every project's daemon), the in-TUI attach picker (deferred
  from ADR-0089), emitting `SessionRemoved` on session teardown, and a web
  control panel frontend.

## References

- [ADR-0089](0089-multi-session-daemon.md) — the registry and handshake this
  extends.
- [ADR-0081](0081-neenee-server-and-attach-model.md) — the attach model and
  discovery record `status` reads.
- [ADR-0017](0017-side-conversations.md) — `ParentStatus`, the single-session
  badge `SessionStatus` generalizes.
- [ADR-0078](0078-round-lifecycle-type.md) — why `SessionStatus` is a derived
  badge, not the round lifecycle.
