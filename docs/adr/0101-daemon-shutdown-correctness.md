# 0101. Daemon shutdown correctness: budgeted state machine, drain, single instance

- **Status:** Accepted
- **Date:** 2026-08-15
- **Builds on:** ADR-0096 (unified daemon), ADR-0100 (lifecycle standard —
  implements its mechanisms), ADR-0025 (SessionEnd hooks), ADR-0054
  (application-neutral host layer)

## Context

A review of the daemon's startup/exit machinery (pre-0101) found the startup
side sound (thin binary → one `host::run` loop, lazy session assembly,
bind-confirmed discovery record) but the exit side built on optimism:

1. **Only SIGINT was handled** (`host::run` awaited `tokio::signal::ctrl_c`).
   `kill <pid>` (SIGTERM — systemd, docker, every supervisor) killed the
   process with no SessionEnd hooks, no discovery-record removal, and a
   stale UDS socket file. ADR-0081 had promised "exits on SIGINT/SIGTERM";
   only the first half was true.
2. **Teardown had no bound.** `shutdown_all_sessions` serially awaited each
   session's `fire_session_end`; SessionEnd hooks are user-configured
   external processes, so one hung hook meant the daemon never exited —
   worse than being SIGKILLed, because the liveness-probe self-heal never
   engaged.
3. **No connection drain.** An attached connection clones the session's
   broadcast `Sender`, so its `recv()` never sees `Closed`; daemon shutdown
   left live connections to die on process exit (a TCP reset to the client).
4. **Task exits were unconfirmed.** `start_server` was fire-and-forget: no
   JoinHandle existed, the UDS socket file was removed inside an unjoined
   accept task, and the integration test asserted its removal after a
   `sleep(100ms)` — a race papered over by wall-clock.
5. **No single-instance guard.** Two racing `ensure_daemon` callers could
   both spawn a daemon; the second `bind_uds` unlinks and rebinds the first
   daemon's socket, silently stealing its local control channel.
6. **Startup errors were lossy.** A TCP bind failure dropped the oneshot
   sender and surfaced at the top level as a bare `RecvError`.
7. **`neenee-server --project` was a fake flag** — parsed, printed in the
   banner, never used (a leftover from the pre-ADR-0096 per-project host).
8. **Detached spawns shared the invoking shell's process group**, so a
   terminal Ctrl-C SIGINTed the "background" daemon.
9. **Accept errors spun hot** (no backoff), and nothing but Ctrl-C could
   stop a daemon remotely.

ADR-0100 (Proposed) had already catalogued the missing *stop verbs* —
idle-exit, a `Shutdown` control verb, version negotiation. This ADR covers
the correctness layer those verbs need to stand on, and lands them together.

## Decision

Shutdown is a **budgeted state transition with owned resources**, not an
await chain. Four mechanisms, all in `neenee-host`:

### 1. One gate, every trigger (`shutdown.rs`)

`ShutdownGate` = a `CancellationToken` + a latched first-wins
`watch<Option<ShutdownReason>>` + a `forced` flag. All triggers —
SIGINT/SIGTERM/SIGHUP (installed *before* any side effect, via
`SignalGuard`), the `Shutdown` control verb, the idle-exit timer, fatal
startup errors — funnel into `gate.request(reason, escalate)`. A second
trigger while draining latches `forced`, and the run loop's graceful phases
each check it and skip the rest. The gate also carries the daemon's build
version for handshake refusal (ADR-0100 rule 4).

`host::run_with_gate` takes the gate as a parameter: production installs
signals, tests inject reasons. The previously untestable exit path now has
end-to-end tests without a single real signal.

### 2. Budgeted phases with deterministic escalation (`host.rs`)

```
trigger → drain begins under [daemon] shutdown_grace_secs (default 10s)
  Phase 1  release the discovery advertisement (BEFORE anything else, so no
           new client discovers a draining daemon)
  Phase 2  cancel listeners → publish MonitorEvent::DaemonDraining → close
           live connections (ConnTable drain, bounded) → join accept tasks
           (TaskBook, bounded) — replacing the sleep(100ms) race with a join
  Phase 3  tear all sessions down CONCURRENTLY, each SessionEnd hook under
           its own deadline derived from the remaining budget (max(hook),
           not sum(hook))
  deadline or forced → abort stragglers (named per task in the log) → RAII
           cleanup still runs → exit
```

Exit code contract: **0** for any completed stop — graceful *or* forced,
signals included (a supervisor's stop succeeding is the normal outcome);
**1** only for startup failures. `RunOutcome` (`Stopped`/`ForcedExit`/
`StartupFailed`) carries this to both binaries.

### 3. Owned resources (`serve.rs`, `serve_discovery.rs`, `persistence`)

- `TaskBook`: named `JoinHandle`s for the accept loops; shutdown *confirms*
  exit and names refusers (`still running` vs `panicked`).
- `ConnTable`: every accepted connection registers a cancel token
  (self-deregistering via a drop guard); drain cancels them and waits
  bounded. Watch clients additionally receive an explicit
  `MonitorEvent::DaemonDraining` frame before the close.
- `DiscoveryLease`: RAII over `daemon.json` — `Drop` removes it on every
  exit path including panics; the graceful path releases it explicitly as
  drain phase 1.
- Single-instance `flock` on `daemon.lock` beside the record, held for the
  process lifetime. A second daemon waits (≤15s) instead of stealing the
  UDS socket — the spawn race is resolved by the kernel.
- Bind failures now travel as real `io::Error`s (`Startup` carries
  `Result<u16, io::Error>`), surfacing as readable fatal startup failures.

### 4. The stop verbs (implements ADR-0100's mechanisms)

- `ControlRequest::Shutdown` + `neenee stop`: acknowledges on the wire
  *before* triggering the drain (the drain would cancel the replier), then
  funnels `ShutdownReason::ControlVerb` into the same gate as signals.
- Idle-exit (ADR-0100 rule 3): zero sessions + zero connections held for
  `[daemon] idle_exit_minutes` (default 5, `0` = never) requests
  `ShutdownReason::IdleTimeout`. A timer as a trigger source — nothing else
  changes.
- Version negotiation (rule 4): the discovery record carries
  `version: CARGO_PKG_VERSION`; `Wire::Select` carries the client's version
  and the daemon refuses a mismatch with a both-versions message naming
  `neenee stop` as the fix. An absent version is served (old-client
  tolerance); a record with no version counts as mismatched for clients.

### Housekeeping on the same sweep

`neenee-server --project` is deleted (hard error now, was a silent no-op);
detached spawns get `process_group(0)`; accept errors back off exponentially
(capped 1s); `[daemon]` config table (`shutdown_grace_secs`,
`idle_exit_minutes`) with CLI overrides on both `neenee serve` and
`neenee-server` (`--grace`, `--idle-exit`); `assets/neenee.service` is the
documented always-on deployment option (`--idle-exit 0`, `TimeoutStopSec` >
the daemon's own grace so the internal force path wins the race with
systemd's SIGKILL).

## Alternatives considered

- **Timeout only at the top level** (wrap the whole teardown in one
  `timeout`): rejected — one slow listener drain would starve every
  SessionEnd hook of its budget. Phases must divide the budget.
- **Keep `publish_for_test` for the drain announcement**: rejected — the
  daemon-scope event is a production surface; it is now
  `SessionRegistry::publish_host_event`.
- **Graceful WebSocket Close frames from the drain path**: not reachable —
  the sink is owned by the per-connection future; cancelling drops the
  socket. Watch clients get the `DaemonDraining` frame instead (a protocol
  signal, stronger than a frame-level close); attach clients treat the
  disconnect per the existing reconnect-with-backoff contract
  (`server-api.md`).
- **`JoinSet` for the task book**: rejected — wrapping the original handles
  loses the ability to name/abort stragglers; the poll-based join is
  trivially auditable against a seconds-scale budget.

## Consequences

- `kill <pid>` and `systemctl --user stop neenee` now run the full graceful
  drain. The ADR-0081 promise is finally true.
- A hung external hook can no longer pin the daemon: worst case it is
  abandoned at its deadline and named in the log; the exit is forced but
  clean (records removed, socket unlinked).
- The wire protocol gains one monitor variant (`DaemonDraining`), one
  control verb (`Shutdown`), one optional `Select.version` field, and the
  discovery record gains an optional `version` — all additive with serde
  defaults; old frames still parse.
- Version skew fails loud: a stale daemon behind a new client refuses with
  instructions instead of mis-serializing mid-session. First deploy of this
  change will mismatch every pre-version record once — by design; `neenee
  stop` resolves it.
- Tests: `serve_integration` loses its `sleep(100ms)` (replaced by a
  bounded `TaskBook` join), gains shutdown-verb / version-skew coverage;
  new `lifecycle_integration` drives the run loop end-to-end (drain
  announcement, discovery removal, idle exit, escalation, readable bind
  failure) with injected triggers.

## References

- [ADR-0100](0100-daemon-lifecycle-standard.md) — the lifecycle standard
  whose mechanisms this implements.
- [ADR-0096](0096-unified-session-daemon.md) — the daemon being fixed.
- [ADR-0025](0025-lifecycle-event-hooks.md) — the SessionEnd hooks the
  budget must bound but still fire.
- `crates/neenee-host/src/shutdown.rs`, `host.rs`, `serve.rs`,
  `serve_discovery.rs`; `crates/neenee-host/tests/lifecycle_integration.rs`.
