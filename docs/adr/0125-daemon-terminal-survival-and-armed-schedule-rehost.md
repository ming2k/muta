# 0125. Daemon terminal survival (setsid) and autonomous-session rehost

- **Status:** Accepted
- **Date:** 2026-08-20
- **Builds on:** ADR-0096 (unified daemon), ADR-0100 (lifecycle standard:
  "the daemon is not the authority — disk is"), ADR-0101 (shutdown
  correctness, signal handling, process-group detach), ADR-0113 (idle
  suspension), ADR-0090 (session-scoped scheduled prompts)

## Context

Two gaps surfaced together in a review of what "the daemon owns every
session" actually promises:

1. **The detached daemon died with the terminal that spawned it.** Both
   spawn paths (`client::spawn_daemon`, the CLI's `detach_daemon`) used
   `process_group(0)` — correctly, per ADR-0101, to escape the shell's
   foreground group — but never left the terminal's *session*. When the
   terminal emulator (or the compositor hosting it) exits, the kernel sends
   `SIGHUP` to every process in that session's controlling group. The
   daemon's ADR-0101 handler treats SIGHUP as "drain gracefully," which it
   did — killing every hosted session on terminal close. tmux's server
   survives the same event because it calls `setsid(2)` before serving; the
   comment in `client.rs` called `process_group(0)` "`setsid`-equivalent",
   which it is not: a new process group ≠ a new session.

2. **`/schedule` autonomy silently depended on nobody restarting anything.**
   The durability chain was sound on paper — jobs are session-scoped state,
   persisted with the transcript, restored on lazy resume (ADR-0090) — but
   three holes broke it in practice:
   - The scheduler task was spawned fire-and-forget in
     `bootstrap::assemble` with no cancellation token, so suspension/kill
     (ADR-0113) leaked it forever: it ticked every 30s against a dead
     channel for the daemon's remaining lifetime.
   - `run_schedule_tick` did `let _ = tx.send(...)` *before* mutating the
     job (advance `next_fire`, drop the once-job, persist). When the channel
     was dead, the send silently failed — the tick still consumed the fire.
     A cron lost one interval per tick; a once-job vanished unrecoverably.
   - Idle-suspension (ADR-0113) and daemon exit both parked the session's
     tick loop with no way for it to come back without a human attaching —
     and the daemon never rehosted anything at boot, so a restart (upgrade,
     crash, reboot) left every armed schedule dormant until attach.

Net effect: a cron meant to fire nightly at 02:00 stopped firing the moment
the daemon restarted or the session idled out, with nothing in any UI saying
why.

## Decision

1. **Detach via `setsid(2)`.** Both daemon spawn paths add `pre_exec(|| {
   setsid() })` alongside `process_group(0)`. A failure of the call is fatal
   for the spawn — a half-detached daemon is exactly the lie "detached"
   cannot afford. SIGHUP remains handled (a SIGHUP that does arrive still
   drains; terminal death just no longer generates one), so operator-visible
   semantics (`kill -HUP`) are unchanged.

2. **The scheduler is session-scoped.** `start_supervised_schedule_scheduler`
   takes an optional `CancellationToken`; the registry passes the hosted
   session's own token (created before the assemble, shared with the driver
   select), so one cancel stops the harness *and* its tick loop.

3. **Dispatch is deliver-first, mutate-second.** `run_schedule_tick` sends
   the prompt and only advances/drops/persists the job if the send
   succeeded. An undeliverable fire leaves the job armed — the exact
   ordering invariant a durable queue needs.

4. **Armed schedules are never idle-suspended.** `suspend_idle_sessions`
   treats a non-empty `scheduled_jobs` as active work: the rehost path and
   this guard together make "armed ⇒ its scheduler is live" an invariant the
   dashboard can rely on.

5. **The daemon rehosts autonomous sessions at boot.**
   `sessions_with_armed_schedules()` (persistence) scans every project
   bucket's snapshot headers — no transcript decode — returning
   `(session_id, project_root)` pairs read from the snapshots themselves
   (bucket names are a one-way hash). `SessionRegistry::rehost_armed_sessions`
   (runtime) re-assembles each through the ordinary lazy-resume path after
   the listener binds, yielding to an early shutdown trigger. A missing
   project root or a failed assembly leaves that session dormant (it still
   lazy-resumes on attach); rehost never blocks startup. Gated by
   `[daemon] rehost_armed_schedules` (default `true`).

## Alternatives considered

- **Rehost every persisted session at boot.** Rejected: it resurrects
  hundreds of transcripts into memory against ADR-0113's whole purpose, and
  only sessions with armed jobs have work that proceeds *unattended*. A
  dormant transcript needs nothing until someone attaches; a dormant
  schedule needs its scheduler.
- **A daemon-independent scheduler process** (cron/systemd timers firing
  `neenee attach`). Rejected: it forks the trust model (a second process
  holding session write access), and ADR-0100 already names the
  service-managed daemon as the blessed always-on posture for users who
  need reboot-proof firing — the rehost makes the default posture correct
  too.
- **Keep SIGHUP-kills-daemon and lean on lazy resume.** Rejected as the
  *default*: it silently stops every running round mid-flight on terminal
  close. Running rounds are not resumable (the interrupted round is
  abandoned by design; only committed turn boundaries survive), so "just
  resume" loses work that "don't die" keeps. Users who want the old
  behavior still have `kill -HUP`.
- **Persist a "round in flight" marker and auto-continue after restore.**
  Explicitly out of scope: an interrupted tool call may have had
  side effects, and auto-replaying it is unsafe. The correct recovery
  contract (docs/explanation/agent-design/session-persistence.md) already
  covers what resume guarantees.

## Consequences

- Terminal/compositor death no longer takes the daemon or its hosted
  sessions with it; `ps -o sess` shows the daemon outside any terminal
  session. Closing the last terminal is now what the docs always claimed:
  a detach, not an exit.
- A daemon restart restores autonomous sessions within seconds of boot;
  their dashboard rows appear as ordinary hosted sessions. Combined with
  guard 4, an armed job's next fire is bounded by its cron interval, not by
  "when the user next attaches."
- Crash-window semantics are unchanged and now honest: a SIGKILL between
  "fire due" and "persist advance" can double-fire a prompt on the next
  host (at-least-once). Deliver-first makes the *loss* case — the only
  unrecoverable one — impossible.
- The scheduler spawn signature changed; `start_schedule_scheduler` /
  `start_supervised_schedule_scheduler` take a fourth `teardown` parameter
  (`None` = process-lifetime, for single-session frontends).
- `neenee daemon status` output is unchanged; rehosted sessions are
  indistinguishable from attached ones by design.

## References

- tmux's server-side detach model (`setsid` + server socket) — the prior
  art for decision 1.
- `docs/explanation/agent-design/session-persistence.md` — the Correct
  Recovery Contract this ADR deliberately does not extend (no round-in-flight
  restore).
- `assets/neenee.service` — the always-on deployment option for
  reboot-proof scheduling (complementary, not required, since the rehost).
