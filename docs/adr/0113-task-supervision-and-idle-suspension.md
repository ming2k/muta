# ADR-0113: Task Supervision and Idle Hosted-Session Suspension

| Status   | Accepted |
|----------|----------|
| Date     | 2026-02-15 |
| Supersedes | — |

## Context

A robustness audit of the daemon found that its crash story was excellent at
the *process* level and absent at the *task* level. Every long-lived task —
session drivers, the per-session monitor tap, round tasks, background
schedulers, accept loops — was spawned fire-and-forget with its `JoinHandle`
dropped. The only `catch_unwind` in the workspace was the tool scheduler's
(which converts a panicking tool into an ordinary tool error; it had a test
proving a panic cannot wedge the scheduler). Consequences, all verified in
code before this ADR:

- **Zombie sessions.** A driver-task panic left the `HostedSession` entry in
  the registry forever. `req_tx` is an *unbounded* channel, so attached
  clients kept queueing requests into memory nobody drained; the control
  plane's `let _ = req_tx.send(...)` returned success on a dead session; the
  TUI sat silent with no error and no `Exit`. Recovery required restarting
  the daemon.
- **Blind drivers.** A monitor-tap panic froze the dashboard row and every
  subscriber's stream while the driver kept running tools and spending
  tokens.
- **Wedged round lifecycle.** A round-task panic skipped
  `RoundLifecycle::finish`, so `is_running()` stayed true forever and parked
  user-input requests never resolved.
- **Silent scheduling death.** A panic in the `/schedule` tick loop stopped
  every cron/countdown job in that session with nothing in any UI.
- **Unbounded memory.** A hosted session with real content was released by
  exactly two paths: explicit kill, or the empty-session reaper (which
  requires *never-persisted, no content*). A multi-project daemon's memory
  therefore grew monotonically with its session history — full transcript,
  agent, MCP runtime, and two tasks per session, forever.
- **Leaked grandchildren.** The bash tool spawned `sh -c` with
  `.process_group(0)` but killed with `child.start_kill()`, which signals
  only the direct child. `sh -c "server & echo hi"` left `server` alive,
  reparented to init. Hooks and MCP wrappers (`npx`, `uvx`) had the same
  shape. An agent that shells out thousands of times over days leaks
  processes into the machine.
- **Frozen event loop.** The TUI's first `@`-completion ran `rg --files`
  *synchronously on the event-loop task* — on a large monorepo, seconds of
  frozen input and rendering.
- **Scrambled terminals.** The TUI had a signal guard restoring the terminal
  on SIGINT/SIGTERM/SIGHUP/SIGQUIT, but no panic hook: any panic unwound the
  process with raw mode and mouse capture still enabled.
- **Variant loops.** The doom guard defaulted to *disabled*, and its
  signature took the raw locator string: `sleep 1; make test` vs
  `sleep 2; make test` were distinct signatures, so the guard could never
  catch the cheapest token-burning loop.

## Options considered

1. **Process-level supervision (restart the daemon on task panic).**
   Rejected: it would kill every *other* hosted session for one session's
   bug, and the daemon is not the layer that owns the semantics of what a
   crashed driver leaves behind.
2. **JoinHandle-based supervision for everything.** Rejected: holding
   handles does not answer the real question — *what should happen to the
   state the task owned?* A tap handle that has panicked tells you nothing
   about the buffer it stopped folding into.
3. **Per-task policy supervision.** Chosen: wrap each task class in the
   policy its state demands (evict / isolate / restart / defer-cleanup),
   with panic payloads surfaced as ordinary errors on the same buses the
   UI already renders.

For memory: reference-counting session lifetime to connections was rejected
(half-open TCP without WS keepalive lies, and detach-keeps-running is
first-class — same reasoning as ADR-0112). A pure TTL reaper extension was
rejected as the *only* mechanism because ending and parking are different
facts. The chosen design is **suspension**: park the session in memory, keep
it fully intact on disk.

## Decision

### 1. Supervision policies (new `neenee-runtime/src/supervise.rs`)

| Task | Policy | On panic |
|------|--------|----------|
| session driver | **evict** | broadcast `AgentResponse::Error` (visible), then tear down via the standard cleanup (entry removal, `Exit`, 2s-bounded SessionEnd hooks, WIP clear, `SessionRemoved`). The session stays durable; the next attach lazy-resumes it. |
| monitor tap | **isolate** | each event folds inside `catch_unwind`; a poison event costs one dropped frame, never the observability path. |
| round task | **defer-cleanup** | the panic becomes `HarnessError::Other`, so the existing tail runs: `close_user_input_round`, terminal `RoundEvent::Error`, `lifecycle.finish`, `Idle`. |
| `/schedule` tick | **restart** | bounded backoff (250ms→15s, 4 attempts), then give up loudly. |
| daemon | **panic hook** | every panic is logged with origin through tracing (a detached daemon has no controlling terminal), then the default hook runs. |

### 2. Idle hosted-session suspension

A hosted session is **suspended** when: no broadcast receivers (no client),
monitor status is not active (not running / awaiting approval / awaiting
input), and no tap activity for 30 minutes. Suspension cancels the driver
and drops the entry — but sends **no `Exit`** and fires **no SessionEnd
hooks**: the session is not over, merely parked; it resumes through the
standard lazy-resume path (transcript, schedule, context all durable). This
bounds daemon memory by *active* work instead of session history.

### 3. Process-group kills

`kill_process_group` signals `-pid` (the group leader set up by
`.process_group(0)`) at every teardown: bash timeout/idle, hook timeout, MCP
transport drop. Grandchildren die with their parents; each site still reaps
(`wait`) with a bounded grace.

### 4. TUI: panic hook and async path scan

- A process-wide `Once` panic hook restores the terminal **only for the
  main thread** (background tasks must not tear down a live grid), then
  chains to the default hook.
- The `@` project scan runs on `spawn_blocking`, hands off through a
  harvest-once slot, and pokes the event loop's dirty signal. Off-runtime
  callers (tests) fall back to an inline scan.

### 5. Doom guard on by default, signatures normalized

`DoomGuardConfig::default()` is now `enabled: true, window: 16` (explicit
`false` in a user's config still wins). Signatures normalize locators —
leading `VAR=value` dropped, `sleep`/`true` segments dropped, bare no-ops
key on their name, program-name and query casing folded, path decorations
trimmed — so variant loops collide while genuinely distinct calls do not.

## Consequences

- A panicking task can no longer produce a silent zombie: every supervision
  path ends in a visible error, a state transition, or a bounded restart.
- Daemon memory is bounded by hosted-and-active sessions, not history.
- Machine process tables stay clean across long agent runs.
- The doom guard's default flip is a behavior change: a model that repeats
  the *same normalized* call in one round now gets a block-and-steer instead
  of a second execution. Per-round blocked-signature masking (ADR-0030)
  already prevents false positives from aging out mid-round.
- Supervision adds `catch_unwind` boundaries; panics inside them unwind
  their own task only, and the daemon-wide hook keeps them observable.

## P2 batch (follow-up)

The second landing batch closed the remaining audit findings:

- **WS keepalive** (attach connections): a 30s ping with a 90s
  peer-silence limit. A peer that dies without an RST used to park the
  connection's read half until TCP's own timeout, holding the session's
  broadcast receiver (and blocking the idle-suspension reaper) for tens of
  minutes. Any inbound frame refreshes the deadline.
- **Lagged resync**: an attach client that falls behind the session bus now
  gets the attach-sync buffer replayed (read-only snapshot — the drain path
  stays reserved for new attachers) instead of a silent `warn + continue`
  that left its view permanently stale.
- **Shell collection caps**: the bash tool caps its in-memory stdout/stderr
  (head+tail at 64k chars with a drop marker) and `lines` (5k, head+tail
  with a `⋯ N lines dropped` marker) at collection time. The text path
  already truncated; the structured payload did not, so `cat huge.log`
  under a 30s timeout could buffer hundreds of MB.
- **Hook output cap**: 1 MiB per stream via `AsyncReadExt::take` — a chatty
  hook can no longer buffer unbounded output for its whole 60s budget.
- **`check_wip` lock scope** (historical; the WIP-coordination tools were
  later removed with ADR-0097 §5's first slice): peer ids resolve outside the
  sessions-map
  lock (the old code awaited `session.id()` with the map held,
  serializing every concurrent resolve/kill/suspend).
- **First-turn AI title re-wired** (ADR-0022): `execute_round` now fires
  the (already timeout-bounded, best-effort) title generator after a
  completed round when the session is untitled and unlocked — the wiring
  had been lost, so AI titles never generated.
- **Skill-mention scan fast path**: the mention grammar (`@`, `skill://`)
  is checked before any text is joined, and the join is windowed to the 32
  most recent user messages. The old unconditional full-history join was
  O(total transcript chars) per call and ran several times per ReAct turn.
- **In-memory input-history cap**: the TUI mirrors the on-disk
  `HISTORY_CAP` so a multi-day session cannot grow the Vec past it.
