# ADR-0132: Session-persisted autopilot posture

- **Status:** Accepted
- **Date:** 2026-08-22
- **Authorization semantics:** ADR-0140; this ADR governs persistence of the
  interaction posture only

## Context

`autopilot` — the posture in which the agent runs without human intervention
(no permission confirmations, no `ask_user`, closed interactive stdin) — was
a **process-local** flag: a `Mutex<bool>` on `PermissionStore`, deliberately
excluded from both `PersistedPermissions` and the session event log
(`docs/explanation/agent-design/autopilot.md` documented this as the
contract). The stated rationale was real: auto-re-granting a blanket
permission posture across a restart, without a human in the loop at restore
time, is a privilege escalation hazard.

But the daemon architecture changed the frame. Since ADR-0096/ADR-0100 the
**disk is the authority** and the daemon "only loses hot state" on death;
ADR-0125 made sessions survive terminal closure and rehost armed schedules at
boot. Under that architecture, the process-local flag produced a concrete
user-facing failure:

> The user is running a long unattended task (`/autopilot on`). The daemon is
> killed — crash, upgrade, reboot, OOM. The session's transcript, todos,
> scheduled jobs, disabled-tool mask, round counter, and `/retry` resume
> point all come back. The posture does not. The user re-attaches and the
> very next side-effecting tool parks on a permission modal — or worse, on a
> session that had walked away, the round stalls silently.

This is exactly the "accidental exit, quick return to the state I left"
failure the session store exists to prevent. Every *other* deliberate
session-scoped choice the user made survives; only this one does not.

The gap was widest on the `--autopilot` startup path, which never wrote the
command ledger either — so not even the legacy ledger heuristic (replaying
the last `Autopilot ON` ack) could recover it, and the user got no notice at
all.

## Decision

1. **The autopilot posture becomes session-scoped persisted state**, following
   the ADR-0048 Phase 2 pattern (`DisabledToolsSet` / `RoundCounterSet`):
   - `SessionData.autopilot: bool` with `#[serde(default)]` (legacy snapshots
     load attended — the historical process-local default — with no
     migration) and `#[serde(default, skip_serializing_if)]` for `false`
     (attended canonical JSON stays byte-identical, so stored checksums
     remain valid).
   - `SessionEvent::AutopilotSet { enabled }`, snapshot semantics, replayed
     by `apply_event` and exported by `snapshot_to_events`.
   - A no-op guard: writing the posture the store already has appends no
     event and rewrites no snapshot.

2. **Every write path persists.** `/autopilot on|off`, the `--autopilot`
   startup flag, and `/principal <role>` (a profile carries its own posture)
   all write the store. Persist failures are logged and never fatal — the
   live flag still flipped, so the worst case degrades to the previous
   process-local behaviour.

3. **Every restore path restores.** The bootstrap resume path (attach,
   lazy-resume, boot rehost — they all flow through `assemble`) reads the
   posture from the store and re-arms the agent before the first round.
   The WS attach snapshot publishes `autopilot` from the store instead of a
   hardcoded `false`, so the badge paints on the first frame. In-process
   `/sessions <id>` switches restore via `restore_session_runtime`.

4. **A posture toggle alone does not materialise an empty session.**
   `is_user_facing_empty` deliberately excludes the flag: arming autopilot on
   a brand-new session with no dialogue creates no file (nothing to resume
   yet); the flag rides along once the session gains substance.

5. **`/reset` de-escalates.** A reset starts a *new* session; the old
   session's posture is not inherited. Fresh data defaults to attended and
   the live agent is re-aligned so an old unattended posture cannot leak
   across the boundary.

6. **Legacy sessions are recovered, not silently re-granted — with a notice.**
   Sessions created before this ADR carry no persisted posture. The restore
   path's ledger heuristic (last `Autopilot ON`/`OFF` ack) now *adopts and
   back-fills* the recovered posture onto the store, with an explicit
   "Autopilot restored (recovered from the command ledger)" notice, so the
   next restart takes the direct path and the heuristic retires for that
   session. This narrows the old compromise (notice-only, manual re-arm)
   because the ledger entry is itself a durable record of an explicit human
   decision made *for this session*, made through the same command surface —
   not a privilege granted by an unrelated process default. `/autopilot off`
   remains one keystroke away and the notice says so.

## Why this is safe where blanket auto-restore would not be

The hazard the old contract guarded against was *silent* re-granting from
state a human never chose. Here:

- The posture is **scoped to the session**, not the project or the process;
  one session's unattended run never widens another's.
- The posture was **explicitly set by a human command** on that very session
  (`/autopilot on`, `--autopilot`, or a `/principal` role choice), recorded
   durably in the same event log as every other deliberate session choice.
- The restore is **loud**: the badge paints immediately (attach snapshot +
  `AutopilotChanged`), and a back-filled restore emits a notice naming the
  recovery source.
- The **de-escalations are also durable**: `/autopilot off` persists, and the
  last write wins — a restart cannot resurrect a posture the user turned off.
- `/reset` and every genuinely fresh session start attended, so the posture
  is never inherited by accident into new work.

This is the same trust line the codebase already draws elsewhere: the
per-project `always` allowlist is persisted and restored across restarts
(`PersistedPermissions`); a per-session blanket posture set by an explicit
command sits on the same line.

## Consequences

- **Positive.** A daemon crash, kill, upgrade, or reboot mid-unattended-task
  reopens the session unattended; combined with the `/retry` resume point
  (ADR-0128), the user is one keystroke from continuing exactly where they
  left off instead of re-granting permissions mid-stream.
- **Positive.** The `--autopilot` startup gap closes: the posture is now on
  the store, so it survives restarts and later attaches.
- **Positive.** The attach snapshot's hardcoded `autopilot: false` is gone —
  the first frame a client sees tells the truth.
- **Negative (accepted).** A session whose files are readable by an attacker
  with write access can now flip the posture bit to have the next resume run
  unattended. That attacker could already rewrite the transcript, the
  allowlist (`permissions.json`), and the config; the posture bit adds no
  meaningful new surface.
- **Negative (accepted).** Restoring into a posture the user forgot was on
  means the resumed session skips prompts. Mitigated by the immediate badge
  and (for the back-fill path) the notice; `/autopilot off` is the escape.

## Alternatives considered

- **Keep it process-local (status quo).** Rejected: the accidental-exit
  recovery story — the entire point of the event-sourced session store and
  the boot rehost — had a one-flag hole right at its centre, and the
  unattended use case is precisely the one where nobody is watching to
  notice the de-escalation.
- **Notice-only on restore (the pre-ADR-0132 compromise).** Rejected as the
  primary mechanism: it covered only the in-process `/sessions <id>` switch
  path (attach and rehost never ran it), relied on fragile title-string
  matching against ledger acks, and required a human to re-arm — which is
  unavailable by definition in the unattended scenario. It survives only as
  the legacy back-fill heuristic for sessions that predate the persisted
  flag.
- **Config-level `autopilot = true` sticky default.** Rejected: a global
  default elevates *every* new session, not the one the user chose. The
  session-scoped flag is strictly narrower.

## References

- ADR-0048 (session as single source of truth; Phase 2 session-scoped
  runtime state pattern)
- ADR-0088 / ADR-0091 (command ack toasts and the durable command ledger —
  the legacy heuristic's data source)
- ADR-0096 / ADR-0100 (unified session daemon; disk is the authority)
- ADR-0125 (terminal survival, boot rehost of armed schedules)
- ADR-0128 (`/retry` resume point — the companion that resumes the *work*)
- `docs/explanation/agent-design/autopilot.md` (updated: the process-local
  paragraph is superseded by this ADR)
