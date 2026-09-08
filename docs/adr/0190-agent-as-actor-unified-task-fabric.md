# 0190. The agent is an actor: one unified task fabric, one mailbox, zero blocking

- **Status:** Accepted
- **Date:** 2026-09-07

> This proposal replaces the earlier "finish the fabric" draft of ADR-0190
> (never Accepted). That draft patched the existing three execution paths;
> this one deletes them. The difference is deliberate: every parallel
> execution path that survives is a second state machine the model must
> guess about, the wire must carry, and the tests must cover. One primitive,
> or none.

## Context

The agent loop blocks. A model asked to start a dev server calls `run_command`
foreground and the turn parks until the 30-minute watchdog
(`crates/muta-agent/src/tools/execute_command/mod.rs`, default
`timeout = 1800`). The diagnosis is not "the background feature is missing" —
the diagnosis is architectural:

1. **Execution is not one system.** The codebase carries five overlapping
   async regimes: episodic foreground commands, sentinel-protocol persistent
   terminals (`execute_command/persistent.rs`, a process-global
   `LazyLock<HashMap>` keyed by `"default"`), background process jobs
   (`BackgroundJobManager`, "Track A"), sub-runner jobs ("Track B"), and the
   scheduled-prompt scheduler (ADR-0090). Each has its own spawn path, event
   shape, control verbs, and lifetime rules. The model sees three of them as
   tool-surface parameters (`background`, `run_persistent`, `terminal_id`) and
   must guess which to use; nothing in the harness enforces the choice.
2. **The mailbox is dead code.** `BackgroundJobManager::outcome_receiver()`
   (`crates/muta-runtime/src/background_jobs.rs:86`) is documented "for the
   session event loop mailbox" with zero callers; every completion outcome is
   sent into a channel nobody reads. `AgentEvent::BackgroundJobCompleted`
   exists only as an orchestration *exit* mapping — no producer. The tool
   response's promise "You will receive an automatic notification when it
   finishes" is false, models learn it is false, and distrust pushes them
   back to blocking foreground calls.
3. **Events reach the UI, never the model.** The forwarder at
   `crates/muta-runtime/src/bootstrap.rs:847` maps job events into
   `RoundEvent::BackgroundJob*`; the TUI shows a human notice, headless drops
   them. The most important consumer of the async substrate — the model — is
   not on the subscriber list.
4. **Foreground has no liveness short-circuit.** ADR-0153's idle budget only
   guards silence-from-birth; a service prints a banner then goes quiet while
   listening, so the guard never fires.
5. **Nothing is durable.** Jobs die with the process; ADR-0125 had to build a
   bespoke rehost for schedules only. Meanwhile the project already owns a
   single-writer SQLite ledger (ADR-0163/0178/0187) purpose-built for exactly
   this kind of authoritative state.

The old draft's answer was to wire the mailbox, add a detach short-circuit,
graft a `JobKind::Service` onto `JobSpec`, and session-scope the terminal
pool. All true observations, all incremental: the five regimes would still
exist, the model would still face a three-parameter execution surface, and
the scheduler would still be a second fabric. Break clean.

## Decision

Rebuild the execution face of the agent as **one actor-grade task fabric**:
every unit of work that executes over time is a Task; the driver loop is an
event loop over a single mailbox; no tool call ever blocks the loop; all
task state is durable; every legacy execution path is deleted, not wrapped.

### D1. One primitive: Task

A single `Task` type replaces process jobs, sub-runner jobs, scheduled jobs,
and interactive terminal sessions:

- `TaskSpec::Process { command, cwd, env, kill_spec, kind, readiness, restart }`
- `TaskSpec::Agent { runner, prompt, policy }` (sub-agents, ADR-0150/0183)
- `TaskSpec::Timer { trigger: Schedule::Cron | Schedule::Once | Interval }`
- `TaskSpec::Terminal { pty, cols, rows }` (interactive PTY sessions)

One registry per session, one lifecycle (`Started → Progress* → Ready? →
Settled{Succeeded|Failed|Killed|TimedOut}`), one control surface
(`task_poll` / `task_logs` / `task_stream` / `task_stop` / `task_wait`),
one event stream, one set of tests. The scheduler (ADR-0090) loses its
private internals — a cron tick is a Timer task settling. The Track A/B
split in `BackgroundJobManager` dissolves — agents and processes are specs,
not species.

`kind: Interactive | Service` refines `Process`:

- **Interactive** (default): bounded work. Running past the sync budget (D2)
  converts it to a spawned task; completing wakes the requester.
- **Service**: long-lived. *Running is the success state.* Emits `Ready`
  when a readiness condition holds (default: first output line after spawn;
  optional TCP port probe or health command). Never settles while running;
  unsolicited death settles `Failed` and wakes the session (crash awareness
  is structural, not a feature). Optional `restart { n, backoff }` policy.
- `Terminal` tasks are session-scoped PTYs — writable, readable, listed in
  the registry, torn down with the session. The sentinel-protocol
  `PersistentTerminalSession` and the global `PERSISTENT_TERMINALS` pool are
  deleted outright; ADR-0137's PTY layer is the only terminal substrate.

### D2. Two-phase tool protocol: the loop never blocks

Every tool execution returns exactly one of:

- `Immediate(result)` — completed within the call, the only shape pure
  read/write tools ever produce; or
- `Spawned { task_id, snapshot }` — accepted into the fabric, results arrive
  via the mailbox.

`run_command` becomes `shell { command, mode: sync | task, detach_on_budget? }`:

- **sync** runs under a short interactive budget (default **120 s**, config);
  on budget expiry it does **not** die — the process converts to a fabric
  task (detach-on-budget) and the call returns `Spawned` with guidance. There
  is exactly one transition path and it is failure-free: the model can never
  again lose a process by choosing the wrong mode, and never again stall a
  round by more than the sync budget.
- **task** spawns directly (`Spawned` immediately). The 1800 s wall-watchdog
  becomes a fabric-level `timeout` policy on tasks, not a loop-blocking
  property of a tool call.

Deleted from the tool surface: `background: true`, `run_persistent`,
`terminal_id` as `shell` parameters. Three guesses the model had to make
collapse into one honest verb. Interleaved streaming (`task_stream`) exists
for the verify-while-watching pattern; streaming to the *model* is opt-in,
streaming to the *UI* is always-on.

### D3. One mailbox; the driver is an event loop

The SessionDriver's loop is a `select!` over exactly four sources: user
input, task events, control-plane requests, and the clock (timers due).
Task events classify at the fabric boundary:

- **Wake-eligible**: `Ready` (Service), unsolicited `Failed` (Service crash),
  `Settled` for tasks the model awaited or that converted from sync. A wake
  injects a harness-authored, model-visible digest entry (task id, spec,
  state, tail summary, log path — ADR-0186 entry kind) and requests a **wake
  turn** at the round boundary (queued if a round is active, per ADR-0126).
- **Progress-only**: streaming lines, UI-destined, never wake the model.

Wake budgeting: at most one pending wake per task; burst coalescing; a wake
never interrupts a round, only feeds its boundary. The dead
`outcome_receiver` channel is deleted — the mailbox is this select arm, and
the old promise ("automatic notification") becomes a wire-level invariant.

### D4. Durable by default; rehost is a policy, not a subsystem

Every task is a ledger row in `muta.db` (single-writer actor, ADR-0178/0187):
spec, owner, state transitions, log pointer, digest. Therefore:

- Daemon restart inspects the ledger: tasks with `restart` policy (services)
  are **rehosted**; terminal tasks are replayed as `Settled` digests on the
  session's next wake; Timer tasks re-arm. ADR-0125's bespoke
  schedule-rehost machinery (header scan, `rehost_armed_schedules`) is
  retired into this general mechanism.
- Session teardown (ADR-0100/0113) settles the task tree: process-group
  SIGTERM→SIGKILL cascade via `muta-platform` process trees, rows closed
  with terminal states, never silently dropped.

### D5. Structured concurrency with explicit detach

Every task has an owner — a turn or the session. Turn-scoped tasks
(sub-agent runs, verification shells) are **cancelled when the turn is
interrupted** unless the tool call passed `detach: true`, which promotes the
task to session ownership. This is nursery semantics (structured concurrency
with an explicit escape hatch): no orphan processes after Esc, no surprise
cancellation of work the model deliberately detached. All fabric channels
are bounded with defined overflow policy (progress lines coalesce under
pressure; lifecycle events never drop un-emitted); the current unbounded
mpsc pattern is retired from the fabric.

### D6. Wire, monitor, and surfaces

`TaskSnapshot` / `TaskEvent` are contracts with ts-rs wire generation
(ADR-0134 rules — additive, no bump). The monitor protocol (ADR-0093)
surfaces the per-session task tree; the TUI gains a task panel (spawn, tail,
stop) and the transcript renders digest entries; the dashboard shows
cross-session services. Permission integration is unchanged in mechanism —
`shell` still submits `HazardLevel::CommandExecution` payloads with
`ProcessKillSpec` (ADR-0146) — but `task_stop` on a spawned task reuses the
kill spec without a second prompt.

### D7. Contract discipline (cross-cutting)

A harness-authored promise without a consumer is a defect of the same class
as an unimplemented public API. Doc comments that promise behavior (mailbox,
notification, guidance text shown to the model) must name or exercise their
consumer; the review checklist and update-checklist gain the corresponding
gate. This ADR's audit found three live drifts (D2/D3); the discipline
exists to end the class.

## Alternatives considered

- **The previous draft of this ADR** (wire the mailbox, detach short-circuit,
  graft `JobKind`, session-scope the terminal pool): rejected as the end
  state — correct on every observation, but it preserves five regimes, a
  three-parameter execution surface, and a scheduler outside the fabric.
  Patching the symptom leaves the state-machine multiplicity that caused it.
- **Polling-first model interaction**: rejected — burns tokens and wall time,
  has no completion trigger; polling remains available (`task_poll`), never
  mandatory.
- **Background-by-default for everything**: rejected — the modal case is fast
  bounded work; `Immediate` within one call is the cheapest protocol for it.
  The two-phase return keeps that fast path while making blocking
  structurally impossible beyond the sync budget.
- **Full coroutine/future passing** (model receives join handles it must
  explicitly await as tool calls): rejected — imposes protocol ceremony on
  every async interaction and re-creates blocking by another name; the
  mailbox wakes the model, not the model's attention.
- **Keep unbounded channels + best-effort events**: rejected — backpressure
  ignorance is how "the UI knows, the model doesn't" drifts happen; bounded
  policies make overload behavior specifiable and testable.

## Consequences

### Positive

- One execution concept across processes, agents, terminals, timers, and
  (future) MCP long-running tools; one state machine to test, one wire
  vocabulary, one control surface.
- The loop is unblockable by construction: worst case is one sync budget
  (120 s default), and the process survives the transition.
- Service crash detection, readiness, and restart become structural
  properties; dev-server workflows become "spawn, get `Ready`, keep working".
- Tasks survive daemon restarts; schedules, services, and long verifications
  share one durability story.
- Structured concurrency ends orphaned-process and zombie-subagent classes;
  bounded channels end silent event loss.
- The fabric is the natural substrate for everything the roadmap already
  wants: MCP task integration, sub-agent fleets (ADR-0150), parallel
  verification, agent-initiated long research — all one inbox.

### Negative / trade-offs

- This is a rewrite, not a patch: `execute_command`'s three paths, the
  scheduler internals, `BackgroundJobManager`, and the terminal pool are
  deleted. The migration is the milestone plan below, and it is honest work —
  accepted deliberately.
- Wake turns add token cost; bounded by coalescing, digest-only payloads,
  and round-boundary delivery.
- Sync-budget conversion changes foreground semantics: a legitimately silent
  bounded command (e.g. a 3-minute compile) detaches at 120 s and its result
  arrives as a wake — a different rhythm than today, and a strictly safer
  one; `mode: task` is the explicit form for known-long work.
- Wire additions (`Task*` types, `Ready` event, two-phase tool return shape)
  are additive per ADR-0134 but touch every frontend.
- Ledger-backed tasks add write volume to `muta.db`; micro-batched
  single-writer commits (ADR-0178/0187) absorb it, and task rows are compact.

### Implementation milestones (each lands independently, deletion is last)

1. **M1 — Fabric core.** `Task`/`TaskSpec`/registry/event stream/bounded
   channels; ledger persistence; process specs only. `BackgroundJobManager`
   delegates to it; no behavior change yet.
2. **M2 — Two-phase tool protocol.** `shell { mode }`, sync budget with
   detach-on-budget conversion; `Spawned`/`Immediate` wire shape.
3. **M3 — Mailbox and wake turns.** The driver select arm; digest entries;
   wake budgeting and round-boundary queueing; `Ready` event.
4. **M4 — Unify the species.** Agent specs, Timer specs (scheduler
   internals retire), Terminal specs on the PTY layer.
5. **M5 — Durability and rehost.** Restart rehost policies; ADR-0125
   schedule-rehost machinery retired.
6. **M6 — Deletion sweep.** Remove `background`/`run_persistent`/
   `terminal_id` parameters, episodic/persistent split, sentinel protocol,
   global terminal pool, Track A/B split, unbounded channels; docs,
   AGENTS.md, and skill guidance updated to the one-verb reality.

### Implementation addendum (2026-09-07, post-acceptance reconciliation)

M1–M3, M5, and M6 landed as specified. M4 landed with two honest
narrowings, both dictated by D7 (a promise without a consumer is a defect):

1. **Timer specs landed; the scheduler did not need retiring.** The
   scheduled-prompt machinery (ADR-0090/0125) had already been deleted in a
   prior tranche (cron/repeat removal); `TaskSpec::Timer` was therefore not
   a retirement but a fresh primitive: one-shot and re-arming timers that
   settle into the ordinary completion path and wake the session through
   the same mailbox as every other task event. No second clock subsystem
   was built.
2. **`TaskSpec::Terminal` and `TaskSpec::Agent` were not added — and the
   ADR is corrected rather than faked.** ADR-0137's "Persistent PTY
   Terminal Sessions" was never a PTY: it was the sentinel-protocol pipe
   pool this ADR deletes. No PTY substrate (no `portable-pty`-class
   dependency) exists in the tree, so a `Terminal` spec would either be a
   rebranded sentinel pool (the exact drift D7 forbids) or a new
   dependency-and-emulator subsystem that deserves its own decision
   record. Likewise, sub-agents already own a strictly richer
   communication channel (ADR-0029's full-duplex runner registry, with
   live permission routing) than the fabric's fire-and-forget event
   stream; folding agents into fabric tasks would be a downgrade, and the
   Track A/B dissolution is complete now that the dead `JobSpec::Runner`
   and its two zero-caller methods are deleted. A future `Terminal` spec
   requires a real PTY dependency decision (follow-up ADR) and lands only
   with the substrate it names.

M5's ledger landed on the unified KV store (ADR-0168): every settle writes
a `task:` row; boot rehost respawns only services whose spec carries a
restart policy (the general successor of ADR-0125's armed-schedule
machinery, which no longer exists to retire).

### D5/D6 disposition (2026-09-07, hardening pass)

**D5 landed fully.** Three mechanisms closed the remaining gaps: (1)
ownership pass-through — `ProcessSpawnOptions.owner_session` +
`SessionJobService::bind_owner` stamp every task entry and ledger row with
its session, so "whose task is this" is protocol-answerable and the rehost
scan can attribute; (2) the last unbounded channel in the fabric is gone —
the legacy spawn path's line pump is bounded (1024 in flight) with a
defined overflow policy: pressure coalesces the *progress broadcast* while
the ring buffer and disk log keep full fidelity, so a slow UI never stalls
a child and a flood never balloons memory; (3) the escape hatch was
already real (`background: true` is the explicit detach; detach-on-budget
is its automatic form), and kill_on_drop + process-group trees + TaskBook
policies (ADR-0113) were already the nursery semantics — no new machinery
was needed, only the wiring above.

**D6 is recorded as a product increment, not an architecture debt.** The
event plane is complete (Started/Progress/Ready/Completed on the wire,
TUI notices, `/jobs` table, poll/logs/kill/wait tools). An interactive TUI
task panel (list/tail/stop affordances) is UX work that adds no structure
and is deferred to the frontend roadmap.

**D6 revisited (2026-09-07, second hardening pass): partially promoted.**
Two facts forced the promotion of the daemon-side half. First, D4's rehost
spawned services into an unobserved manager — their events had no
subscriber, a D7-class drift. Second, daemon-level tasks (owner-less,
restart-surviving) had **no human-side control plane at all**: stopping a
runaway rehosted service required asking the model or restarting the
daemon. This pass therefore landed:

- **`SessionRegistry` daemon-task hub**: a registry-scoped fabric whose
  events fold into a snapshot cache and publish as monitor diffs.
  `spawn_daemon_task` / `stop_daemon_task` are the operator verbs; the
  rehost path now spawns through the hub (closing the unobserved-manager
  gap).
- **Monitor protocol extension** (additive per ADR-0134):
  `MonitorSnapshot.tasks` + `MonitoredTask` rows, and
  `MonitorEvent::TaskUpdated`/`TaskRemoved` diffs. `muta status`
  renders a daemon-tasks section (tested); clients upsert via the new
  `upsert_task_row`.
- The **interactive TUI panel** remains deferred to the frontend roadmap —
  its data plane now exists; only the view surface is missing, and that
  surface should follow ADR-0141/0172/0173 governance as its own ADR.

**Timer gained its model-facing consumer** (closing the D7 gap this pass
itself created): `run_command { schedule_in_secs, repeat }` arms a Timer
task through `BackgroundJobService::spawn_timer`; the schema test locks
the surface.

## References

- ADR-0090 (scheduled prompts — Timer tasks retire its internals),
  ADR-0096/0100 (daemon and lifecycle — task lifetime is session lifetime),
  ADR-0113 (supervision, process-group kills, idle suspension), ADR-0125
  (rehost precedent — retired into D4), ADR-0126 (round-boundary queueing),
  ADR-0134 (wire compatibility), ADR-0137 (PTY sessions, request zoning),
  ADR-0146 (hazard model, ProcessKillSpec), ADR-0150/0183 (agent hierarchy
  and kernel — Agent specs), ADR-0153 (watchdog budgets — superseded in
  spirit: the wall moves from the tool call to the task policy), ADR-0163/
  0178/0187 (ledger — task rows), ADR-0186 (transcript entries — digest
  kind).
- Prior art: Erlang/OTP supervisors and mailboxes; Kotlin structured
  concurrency and nurseries; BEAM `Task.async`/`Supervisor` restart
  policies; MCP tasks specification; Claude Code background shells and task
  notifications; systemd service semantics (`Type=notify`, `Restart=`).
