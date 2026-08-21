# 0130. Native platform capability boundary

- **Status:** Accepted
- **Date:** 2026-08-21

## Context

The daemon and persistence layers mixed platform-neutral lifecycle rules with
Unix mechanisms. Unix domain sockets, `flock(2)`, process groups, `setsid(2)`,
shell selection, file modes, and `/proc` checks appeared directly in business
modules. Several non-Unix branches returned success without enforcing the
operation. In particular, process locks became no-ops, process-tree teardown
killed only the direct child, and daemon liveness always returned false.

That shape could compile portions of the workspace on Windows while violating
the single-instance, cleanup, persistence, and security contracts. XDG also
described both the semantic file categories and their Linux placement, making
the conceptual model appear Linux-specific even though native macOS and
Windows locations already existed.

## Decision

Introduce a leaf `neenee-platform` crate containing small semantic
capabilities. Business and persistence modules depend on those capabilities,
not on OS APIs or a global platform object.

Use these native mechanisms:

- Unix domain sockets with owner-only filesystem permissions on Unix, and
  local Named Pipes with a protected current-user DACL on Windows.
- `flock(2)` on Unix and `LockFileEx` on Windows for equivalent process locks.
- Unix process groups and Windows kill-on-close Job Objects for subprocess
  trees owned by a task. Windows children start suspended, enter the Job Object,
  and resume only after containment succeeds; one spawn API makes that ordering
  an invariant rather than a caller convention.
- `setsid(2)` on Unix and detached Windows process creation flags for daemons.
  Daemons never enter the owned-child Job Object.
- POSIX shells on Unix and non-interactive PowerShell on Windows for explicit
  user script text. Internal operations continue to execute argv directly.
- Owner-only modes on Unix and explicit current-user DACLs on Windows for
  sensitive files. Atomic replacement uses each OS's replace-existing
  primitive.

Keep the Config, Data, State, Cache, and Runtime classifications from
ADR-0014 as platform-neutral semantics. Resolve their default locations with
native OS conventions. XDG variables remain supported as explicit portable
overrides; they are not Windows defaults. Use one application identity
component (`neenee`) so Windows does not duplicate organization/application
segments. Windows state and daemon instance records use LocalAppData rather
than the roaming profile.

Represent local discovery with one tagged `LocalEndpoint`; include a stable
instance-root hash in Windows pipe names and pair each PID with its OS process
creation token (`/proc` start ticks on Linux, `proc_pidinfo` start time on
macOS, `GetProcessTimes` on Windows). Read the historical `uds_path` field
during a compatibility window, but write and consume the native endpoint.
Preserve loopback TCP for the browser and exposed clients.

Require Windows workspace checks and tests before merge. Publish an MSVC zip
with a checksum and provide a checksum-verifying PowerShell installer. A
platform is supported only when its implementation, CI gate, release artifact,
and installation path all exist.

## Alternatives considered

### Keep `cfg` branches in each business crate

Rejected. This duplicates policy, spreads unsafe OS calls, and makes silent
fallbacks hard to audit.

### Use TCP loopback as the only local transport

Rejected. A port is globally discoverable and its access boundary requires a
separate credential. Native local IPC provides OS-enforced per-user access and
avoids port contention for the primary control channel.

### Treat Windows as best-effort

Rejected. Successful no-ops are data-corruption and lifecycle bugs, not
degraded support. Unsupported capabilities must fail at compile time or
startup.

### Make XDG the default on every OS

Rejected. XDG is the native Linux convention, not a universal placement
standard. The useful part is the category model; default locations belong to
the host OS.

## Consequences

Unix behavior remains native while Windows gains equivalent locking, IPC,
process containment, private-file, and daemon lifecycle guarantees. Business
modules become easier to test because transport and lifecycle mechanisms no
longer define their policy.

The platform crate contains carefully reviewed unsafe adapters. Windows CI is
the executable contract for those adapters because non-Windows hosts cannot
run their behavior tests.

Discovery records temporarily contain both the new endpoint and the legacy
Unix field. A later compatibility release can remove the legacy writer and,
after the supported upgrade window, its reader.

## References

- [ADR-0014](0014-xdg-persistence-architecture.md)
- [ADR-0096](0096-unified-session-daemon.md)
- [ADR-0129](0129-single-primitive-unix-daemon-detachment.md)
- [Paths reference](../reference/paths.md)
- [Server WebSocket API](../reference/server-api.md)
