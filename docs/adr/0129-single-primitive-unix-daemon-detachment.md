# 0129. Single-primitive Unix daemon detachment

- **Status:** Accepted
- **Date:** 2026-08-21
- **Supersedes:** ADR-0125 decision 1 only; its scheduler and rehost
  decisions remain accepted

## Context

ADR-0125 correctly required detached daemons to leave the invoking
terminal's session, but its first decision prescribed `setsid(2)` alongside
the existing `process_group(0)` configuration.

Those operations are mutually exclusive in that order. Rust implements
`process_group(0)` with `setpgid(0, 0)` before the `pre_exec` callback. The
child therefore becomes a process-group leader, and POSIX requires
`setsid(2)` to fail with `EPERM` when called by a process-group leader. Rust
reports a failing `pre_exec` callback as a spawn failure, so both explicit
`neenee daemon start` and on-demand daemon startup failed before `exec`.

The two production spawn paths also carried independent copies of the
detachment setup. That duplication allowed lifecycle semantics and tests to
drift: the end-to-end inheritance test reproduced only the old
`process_group(0)` behavior and therefore did not exercise the failing
production combination.

## Decision

1. Configure Unix daemon detachment with `setsid(2)` alone. It creates both
   a new session and a new process group; no preceding or subsequent
   `setpgid(2)` call is needed.
2. Keep the `setsid(2)` call in a minimal `pre_exec` callback. Treat failure
   as fatal because continuing with a half-detached daemon violates the
   daemon lifecycle contract.
3. Centralize the configuration in one runtime helper used by both explicit
   detached startup and on-demand auto-start.
4. Pin the contract with a process-level test that verifies the spawned
   child's session ID and process-group ID both equal its process ID. Make
   the end-to-end daemon lifecycle test reuse the production helper.

ADR-0125 decisions 2–5, covering schedule teardown, deliver-first mutation,
idle-suspension exemption, and boot-time rehost, remain unchanged.

## Alternatives considered

- **Call `setsid(2)` before `setpgid(2)`.** Rejected: `setsid(2)` already
  creates the required process group, and a session leader cannot
  subsequently move process groups. The second syscall has no valid role.
- **Keep only `process_group(0)`.** Rejected: a new process group does not
  leave the terminal session and therefore does not satisfy terminal-death
  survival.
- **Ignore `setsid(2)` failure and continue.** Rejected: the command would
  claim to be detached while remaining coupled to the terminal.
- **Maintain separate implementations for the two spawn paths.** Rejected:
  process lifecycle policy is one invariant and must have one implementation.

## Consequences

- Daemon startup succeeds on Unix while retaining terminal and compositor
  survival.
- The spawned daemon is both a session leader and a process-group leader by
  construction.
- A future attempt to combine `setpgid(2)` and `setsid(2)` fails the focused
  process-level test and the end-to-end daemon lifecycle test.
- Non-Unix behavior is unchanged.

## References

- ADR-0101: daemon shutdown correctness and process-group isolation
- ADR-0125: daemon terminal survival and autonomous-session rehost
