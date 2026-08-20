# 0119. CLI verb vocabulary and lifecycle budget coordination

- **Status:** Accepted
- **Date:** 2026-08-19
- **Builds on:** ADR-0096 (unified daemon), ADR-0100 (lifecycle standard),
  ADR-0101 (shutdown correctness), ADR-0105 (loopback auth)

## Context

By 0.27 the command surface had accreted four overlapping spellings for one
daemon and its sessions, and the parser lived in the wrong crate:

- `serve` vs `daemon start` (two flag tables, maintained in parallel — the
  `--expose`/`--public` usage drift was this duplication's first bug).
- `stop`/`status`/`panel` at the top level *and* under `daemon`, with
  `daemon status` and `status` differing in flags.
- `attach` vs `resume` vs `session attach`, where `attach` with no id
  degenerated to printing a session list on **stderr** and exiting — the
  TUI's sessions-picker modal existed but was never wired to that path.
- `--single-instance`: parsed, then discarded by `main`; the registry
  hardcoded `false`. A dead flag surviving two architectural shifts.
- The whole parser (`parse_args`, help text, completions) lived in
  `neenee-runtime`, a library crate, putting a frontend concern under the
  session runtime's layering.

Separately, a real lifecycle defect: `client::stop` escalated SIGTERM after
a hardcoded 2s, but any signal arriving mid-drain flips the daemon's
`ShutdownGate` to *forced* (ADR-0101) — so a daemon with legitimately slow
SessionEnd hooks (its configured grace is 10s, hooks may take 5s) was
force-killed by the very client that asked it for a graceful stop. The
daemon's drain budget was invisible to the stopper: the discovery record
carried a version but not a grace.

## Decision

**One noun per resource, one verb per action; retirement teaches.**

| Concern | Canonical | Retired → error that teaches |
|---|---|---|
| daemon lifecycle | `neenee daemon start [--fg] / stop / status` | `serve`/`stop`/`status` (top level) |
| joining a session | `neenee attach [id]` | `resume`, `exec`, `--attach` (normalized) |
| session management | `neenee session ls / rm` | `session attach`/`dashboard` (they live on the verbs) |
| one-shot run | `neenee run <prompt>` | `exec` |

- `daemon start` **detaches by default**; `--fg` is the supervisor shape
  (systemd/tmux). The self-spawn (`client::spawn_daemon`) and `--detach`
  both re-enter `daemon start --fg`.
- `attach` with no id **always ends in the TUI picker**: a new wire action
  `AttachAction::Picker` assembles a throwaway carrier session
  (`SessionStart::Picker`), the client's TUI raises the sessions modal,
  and `/sessions <id>` re-attaches through the existing switch path. The
  daemon's `Attach(None)` auto-bind of a lone session is unchanged.
- **The parser moved to `neenee-cli::cli`** as a declarative spec table:
  one `Spec` list drives parsing, help, "did you mean", and the three
  shell completions. `neenee-runtime::startup` keeps only what the
  session runtime needs (`SessionStart`, the slash-command tables,
  `init_tracing`, `DEFAULT_SERVE_PORT`); its `StartupMode` shrank from 24
  variants to the 4 assembly-relevant shapes.
- `--single-instance` is refused with an explanation; its plumbing
  (`BootstrapParams::single_instance`, `Bootstrap::process_lock`) is
  deleted.

**Lifecycle budgets are coordinated through the discovery record.**

- `Discovery` gains `grace_secs`: the daemon publishes its configured
  drain budget when it writes the record.
- `client::stop` derives every tier's wait from it: verb → wait
  `grace_secs`; SIGTERM → wait the same; SIGKILL → 1s. Records predating
  the field fall back to a 15s constant (generous against the 10s
  default, so a legacy record cannot cause an early escalation either).
- Tier 4's UDS unlink gains a pid guard (`uds_belongs_to_pid`): the socket
  is removed only when the recorded daemon is dead *and* nothing answers
  on the path — mirroring the record's `remove_if_matching_pid`, so a
  successor daemon spawned during the stop window cannot lose its socket.
- `wait_for_lock`'s budget is `max(grace, 10s) + 5s`, replacing the
  hardcoded 15s whose comment claimed (falsely) to be "a fraction of that
  daemon's own grace budget". The floor exists because the *predecessor's*
  grace is not knowable from the lock file; the original 15s was sized
  for exactly this and must not regress.

## Alternatives considered

- **Keep the top-level spellings as silent aliases.** Rejected: four
  spellings for one noun is exactly the ambiguity being removed, and
  silent aliases keep doc drift alive forever. Retirement errors that
  name the canonical form cost one rerun and teach immediately.
- **Clap.** Rejected for now: the declarative table keeps the binary
  dependency-free, keeps the error strings in our voice, and completions
  generated from the same table closed the real defect (flag drift). If
  the surface grows command-specific argument grammars, revisit.
- **A `ControlRequest::Shutdown` reply carrying the drain budget.**
  Rejected as the primary channel: the verb may be undeliverable
  precisely when the daemon is wedged (that is when tiers 2/3 fire), and
  the record is readable in every tier without a round-trip. The reply
  remains a possible refinement, not a dependency.
- **Escalate from the client with a second `Shutdown` verb instead of
  SIGTERM.** Rejected: after the first verb the daemon may be unable to
  read the second (wedged hooks); OS signals are the honest floor.

## Consequences

- Wire change: `AttachAction::Picker` (`"picker"`). Older daemons refuse
  the unknown variant with a deserialize error on the Select frame —
  acceptable because the version handshake (ADR-0100 rule 4) already
  refuses skewed pairs before any session work.
- `Discovery` gains a serde-defaulted optional field; older readers
  ignore it, older writers leave it absent (stop falls back).
- The systemd unit (`assets/neenee.service`) moves to
  `daemon start --fg`; `TimeoutStopSec` guidance is unchanged.
- Docs that named `serve`/`stop`/`status`/`resume`/`exec` now name the
  canonical forms (`docs/reference/cli.md`, both READMEs, the two daemon
  how-tos).
- `SlashContext::startup` remains (as `&SessionStart`) for handlers that
  need the start shape; no handler currently reads it.

## References

- ADR-0100 (lifecycle standard), ADR-0101 (drain phases and the gate)
- `crates/neenee-cli/src/cli.rs` — the spec-table parser
- `crates/neenee-runtime/src/client.rs` — `stop()` tier budget, Tier-4 guard
- `crates/neenee-runtime/src/serve_discovery.rs` — `grace_secs`
