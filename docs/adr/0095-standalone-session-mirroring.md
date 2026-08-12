# 0095. Standalone sessions mirror into the host's observability surface

- **Status:** Superseded by ADR-0096
- **Date:** 2026-08-08
- **Builds on:** ADR-0089 (multi-session host), ADR-0093 (monitor protocol),
  ADR-0094 (serve vocabulary)

## Context

ADR-0093 gave the host a control plane — but only for sessions the host
*drives*. A standalone `neenee` TUI (the default invocation) assembles its
own harness in-process and is invisible to `neenee status`: two sessions in
one workspace show one row. That split-brain is the exact problem the
monitor protocol was built to solve, and users hit it immediately ("I have
three neenee sessions; why does the panel see one?").

The obvious fix — make bare `neenee` an attach client, so the host owns
every session — changes the session **ownership model**: the TUI would no
longer hold the session it drives, startup would always pay host-spawn cost,
and a TUI crash would leave a running session behind. That is a larger
product decision (deferred deliberately; see Consequences) and unnecessary
for the actual gap, which is *visibility*, not *ownership*.

## Decision

A standalone `neenee` process **mirrors** its session into the project's
host when one is (or becomes) reachable. Ownership is unchanged: the local
process drives the session; the host receives a read-only status stream.

### 1. Mirror handshake on the same socket

`AttachAction` gains a `Mirror` variant. A mirror client sends
`Select{action: Mirror}`, then exactly one `Wire::Mirror(MirrorHello)` —
the session's static identity (`session_id`, `overview`, `created_at`,
`message_count`) — then streams `Wire::MirrorUpdate(MonitoredSession)` rows.
The channel is strictly client → server; the server sends nothing after the
handshake.

```rust
enum AttachAction { New, Attach(Option<String>), Monitor(MonitorAction), Mirror }

struct MirrorHello { session_id, overview, created_at, message_count }
```

### 2. Mirrored rows are first-class but marked

`MonitoredSession` gains `hosting: SessionHosting` (`hosted` | `mirrored`;
serde-defaults to `hosted` so ADR-0093-era producers stay valid). The
registry keeps mirrored rows in a separate `mirrors` map keyed by session
id; monitor snapshots merge hosted + mirrored rows, with **hosted winning
any id collision** — a mirrored row is a report about a session, a hosted
row is the session itself. Panels render the distinction (`hosted` vs
`⇢ mirror` in `neenee status`); attaching to a mirrored session through
this host fails as before, because the host does not hold it.

The server pins identity fields from the adopted `MirrorHello` onto every
update: a mirror connection can only ever describe the session it adopted,
so one client cannot impersonate or overwrite another session's row.

### 3. Liveness is the truth mechanism

A mirrored row exists exactly as long as its connection. The mirror client
streams updates over a `watch` channel that coalesces bursts to the latest
row; when the owning TUI exits, the tap drops, the supervisor closes the
socket, and the host publishes `SessionRemoved` (the ADR-0093 variant, now
actually emitted). A panel therefore never displays a silently-stale mirror
— the failure mode of "cache the row and hope" is designed out rather than
documented.

### 4. Best-effort, zero-interference client

Mirroring is implemented in `neenee-cli` as a tap threaded into the
driver→TUI response pipeline (`tee_responses`): each response is folded into
a local `MonitorTracker` (the same state machine the host uses, so one
derivation vocabulary exists) and published to the supervisor's `watch`
channel. The supervisor owns discovery (read the discovery record, 2s retry
when absent), connect, reconnect (500ms backoff), and shutdown. Properties:

- **No host is a normal state.** Nothing is logged as an error; nothing
  blocks; the session runs identically with or without a host.
- **The UI path can never stall on mirroring.** The tap is one mutex + one
  fold; the forward channel is unbounded; all network I/O lives in the
  supervisor task.
- **Adoption is idempotent and lossless.** On (re)connect the supervisor
  re-announces the identity and re-sends the current row, so a host restart
  loses nothing but the connection gap. Adoption *stamps* the real session
  id onto the tracker (`rebind_identity`) instead of re-seeding it, so a
  round that started before the host appeared still reads Running on the
  panel — folded state is never discarded for lack of a host.
- **In-process session switches re-adopt.** `ConversationReplaced` (the
  harness's authoritative "the TUI now drives another session" signal,
  emitted by `/session open`, `/new`, fork) triggers a rebind: the tap
  reseeds its tracker and the supervisor sends a fresh `MirrorHello` on the
  live connection, which removes the old row and adopts the new identity. No
  ghost rows across session switches.

## Alternatives considered

- **Make bare `neenee` an attach client (full unification).** The end-state
  candidate — but it changes ownership, startup cost, and crash semantics
  for the default invocation. Deferred as a separate decision; mirroring
  delivers the visibility value at ~5% of the blast radius and keeps that
  door open (a future unification deletes the mirror path, nothing else).
- **Standalone processes register via the filesystem** (drop a record into a
  shared directory; host polls). Rejected: polling invents a staleness
  problem (crashed process leaves a record) that the connection-liveness
  model simply does not have, and it splits observability across two
  transports.
- **Mirror the raw `AgentResponse` stream and derive rows on the host.**
  Rejected: it would double the tracker vocabulary (one derivation for
  hosted sessions, one for mirrored) and leak far more than status over the
  wire. Deriving at the source keeps the wire minimal and the derivation
  singular.
- **Do nothing; tell users to always `attach`.** Rejected: the default
  invocation staying invisible makes the control plane a lie by omission.

## Consequences

- **Positive.** `neenee status` sees every session in the workspace,
  regardless of how it was started — the control plane finally matches the
  user's mental model ("all my neenee sessions").
- **Positive.** The ownership model is untouched: no startup-cost change, no
  crash-semantics change, no new failure mode when no host runs.
- **Positive.** `SessionRemoved` is now exercised in production, completing
  the ADR-0093 event contract.
- **Negative (bounded).** A mirrored row's `overview`/`created_at` are only
  as fresh as its hello; the local transcript preview is not re-sent on
  title changes. Accepted: panels use overview as a hint, and the id is the
  join key.
- **Neutral.** Mirrored sessions remain non-attachable through the host.
  Making them attachable would require the host to *take over* a running
  local driver — a deliberate non-goal.
- **Follow-up (not in this ADR).** Evaluate full unification (`neenee` ≡
  `neenee attach`) after serve-mode mileage: if adopted, mirroring is
  deleted and this ADR is superseded.

## References

- [ADR-0093](0093-daemon-observability-monitor-protocol.md) — the monitor
  protocol and `SessionStatus` vocabulary this feeds.
- [ADR-0089](0089-multi-session-daemon.md) — the registry; ADR-0018's
  one-writer invariant is preserved (the mirror channel is read-only).
- [ADR-0017](0017-side-conversations.md) — the `ParentStatus` precedent for
  derived display state.
