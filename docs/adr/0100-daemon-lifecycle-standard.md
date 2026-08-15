# 0100. Daemon lifecycle standard: on-demand start, scoped shutdown

- **Status:** Accepted (mechanisms landed by ADR-0101)
- **Date:** 2026-08-14
- **Builds on:** ADR-0096 (unified daemon), ADR-0094 (verb vocabulary),
  ADR-0099 (daemon vocabulary)

## Context

The session daemon's lifecycle is currently half-specified:

- **Start** is automatic and invisible: the first client that needs a
  session transparently spawns the daemon (`client::ensure_server`), or the
  user runs `neenee serve [--detach]` explicitly. `neenee status` never
  spawns. This half is done and correct.
- **Stop** is manual only: Ctrl-C on a foreground `serve`, or `kill` on the
  pid from the discovery record. There is no automatic exit path (zero
  sessions and zero clients changes nothing), no remote shutdown verb on the
  control plane, and no defense against version skew — a daemon started
  weeks ago by an older build keeps serving newer clients over a wire
  protocol (`serve::Wire`) that has no version negotiation.

Prior art for per-user background processes, surveyed 2026-08-14:

| Tool | Start | Stop | Lesson |
|------|-------|------|--------|
| tmux | first `tmux` spawns the server | **exits when the last session dies**, never on client detach ([man page](https://man7.org/linux/man-pages/man1/tmux.1.html): "Once all sessions are killed, tmux exits") | lifetime anchors to *state* (sessions), not *views* (clients) |
| gpg-agent / ssh-agent | spawned on demand by the tool ([man page](https://www.gnupg.org/documentation/manuals/gnupg24/gpg-agent.1.html)) | persists; [auto-stop variants](https://lists.gnupg.org/pipermail/gnupg-devel/2016-September/031553.html) and pulseaudio's `exit-idle-time` show idle-exit is a mainstream knob | on-demand spawn needs no service manager |
| LSP servers | spawned by the editor | client-managed (`shutdown` request + `exit` notification) | wrong here: neenee sessions outlive any client |
| Docker / containerd | system service | service manager | wrong here: single-user tool, not infrastructure |
| ollama | [install script offers a systemd unit](https://docs.ollama.com/linux) | service manager | always-on is a *deployment option*, not the default posture |

Two constraints are uniquely neenee's:

1. **Sessions have background value while detached.** Cron-scheduled prompts
   (ADR-0090), long-running tools, and re-attachable state all require the
   daemon to outlive every client. Client-scoped shutdown (exit when the
   last client disconnects) is therefore *harmful*, not merely suboptimal.
2. **The daemon is not the authority — disk is.** Sessions are event-sourced
   and lazily resumed on attach, so daemon death loses only hot state.
   Automatic shutdown is safe by construction.

## Decision

(Proposed — options retained below for discussion.)

Adopt option A. The lifecycle standard has four rules and three mechanisms:

**Rules**

1. **Start is invisible.** Unchanged: on-demand spawn by the first client;
   `neenee serve [--detach]` explicit; observers (`status`) never spawn.
2. **Lifetime anchors to hosted state, never to clients.** Client
   disconnects never affect the daemon.
3. **Empty is exit-worthy, with grace.** When the daemon hosts zero sessions
   *and* has zero attached clients (including monitor subscribers) for a
   continuous grace period (default 5 minutes, `[daemon] idle_exit_minutes`,
   `0` = never), it shuts down gracefully — the same teardown path as
   Ctrl-C (registry teardown, SessionEnd hooks, discovery-record removal).
   The grace period prevents spawn/exit flapping between back-to-back
   invocations; the config escape hatch serves always-on deployments.
4. **Upgrade never silently mismatches.** The discovery record and the
   `Select` handshake carry the build version; a client that detects a
   mismatch refuses with an actionable message naming both versions and the
   restart command.

**Mechanisms**

- **Discovery record v2**: add `version` (the daemon's `CARGO_PKG_VERSION`)
  to `daemon.json`; readers tolerate its absence (legacy record → "unknown"
  → treated as mismatched, prompting a restart).
- **Version handshake**: `Wire::Select` gains an optional client `version`;
  the daemon replies `Wire::Error` with a both-versions message on
  inequality, before any session work. Exact equality is deliberate: the
  wire protocol is pre-1.0 and evolves every release.
- **`Shutdown` control verb**: `ControlRequest::Shutdown` stops accepting
  new attaches, runs the graceful-teardown path, and exits 0 — giving
  scripts, the TUI, and the upgrade flow a clean remote stop that today
  requires `kill`.

**Upgrade flow** (the composition of the above): a mismatched client's error
tells the user to run `neenee serve --detach` after stopping the old daemon
via the `Shutdown` verb; no self-replacing daemon is built.

## Alternatives considered

- **B. Service-managed by default** (systemd `--user` unit / launchd agent
  as the blessed posture, clients never spawn). Rejected as the *default*:
  neenee is a tool, not infrastructure (ADR-0005's single-user workstation
  posture); the spawn-on-demand path already makes the daemon invisible, and
  OS-specific unit machinery is maintenance surface with no payoff for the
  default user. Retained as a documented deployment option: `neenee serve`
  already foregrounds cleanly under any supervisor with
  `idle_exit_minutes = 0`; a sample unit belongs in a how-to, and this is
  the right answer for users who want a hard guarantee that cron-scheduled
  prompts fire after a reboot.
- **C. Single-binary self-re-exec** (tmux's artifact model: `neenee`
  re-execs itself detached instead of spawning the sibling `neenee-server`
  binary). Eliminates spawn-path version skew by construction and halves the
  install surface; cheap now that `neenee-cli` is a thin shell (ADR-0098).
  Deferred, not rejected: it forces a decision about daemon-wide identity
  (today `neenee serve` and `neenee-server` run the host with *different*
  identities per ADR-0054) and links the TUI into the daemon image. Revisit
  if single-artifact distribution becomes a goal.
- **D. Client-scoped shutdown** (exit when the last client detaches).
  Rejected outright: it kills detached cron jobs and long-running tools,
  fires every session's SessionEnd hooks on what feels like closing a
  window, and destroys the resume value that ADR-0096 exists to provide.

## Consequences

- Zero hosted sessions stops meaning "run forever": the default daemon
  becomes truly invisible infrastructure — born on demand, gone when
  useless. Users who never think about the daemon get resource hygiene for
  free.
- The wire protocol gains a version field; legacy daemons without one are
  detected as mismatched and the error path is the fix. This is the only
  user-visible behavior change, and it fails loud, not silent.
- Always-on users set one config key or run one unit file; nothing else
  changes for them.
- `Shutdown` completes the control-plane verb set: create / prompt /
  interrupt / resolve-permission / kill-session / **shutdown** (host).

## References

- [ADR-0096](0096-unified-session-daemon.md) — the daemon and control plane
  this standardizes the lifecycle of.
- [ADR-0090](0090-scheduled-prompt-unification.md) — cron on sessions, the
  reason client-scoped shutdown is harmful.
- [ADR-0025](0025-lifecycle-event-hooks.md) — SessionEnd hooks fired on
  graceful teardown.
- [tmux(1)](https://man7.org/linux/man-pages/man1/tmux.1.html),
  [gpg-agent(1)](https://www.gnupg.org/documentation/manuals/gnupg24/gpg-agent.1.html),
  [ollama Linux install](https://docs.ollama.com/linux) — the surveyed prior
  art in the table above.
