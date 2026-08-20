# 0121. Instance isolation for development and testing

- **Status:** Accepted
- **Date:** 2026-08-20

## Context

neenee resolves every path it writes through the XDG stack
(`docs/reference/paths.md`): config, credentials, OAuth tokens, sessions,
skills, logs, and — since ADR-0096 — the daemon's runtime files
(`daemon.json`, `daemon.sock`, `daemon.lock`) under `$XDG_RUNTIME_DIR/neenee`.
The stack has per-category overrides (`NEENEE_CONFIG_DIR` and friends) but no
single switch, and the daemon's default TCP port is the fixed well-known
9800 (ADR-0105).

A checkout of this repository is simultaneously *a client of the installed
neenee* and *a build of neenee itself*. Before this ADR, running the debug
build (`target/debug/neenee`) or its test suites against a machine where the
installed daemon is running meant, by default:

- a dev `daemon start` contends for the same `daemon.lock`, unlinks and
  rebinds the same `daemon.sock`, and overwrites the same `daemon.json`
  (last writer wins — the record then routes *every* client, installed or
  dev, to whichever daemon wrote last);
- when port 9800 is taken the daemon **silently** falls back to an ephemeral
  port (ADR-0105), so two daemons can coexist while each client population
  discovers only one of them;
- dev sessions, `/reload` config edits, OAuth refreshes, and logs land in the
  same `~/.local/share/neenee`, `~/.config/neenee`, and
  `~/.local/state/neenee` the host installation uses — a debugging session
  can corrupt or at minimum pollute real user data;
- a test that forgets its own tempdir sandbox writes into the real home the
  same way. Each such leak has historically been patched reactively
  (`set_test_default`, the `test-path-override` feature, per-suite
  `NEENEE_*_DIR` sandboxes — see the regression notes in
  `trusted_projects.rs` and `neenee-agent/Cargo.toml`).

The pieces existed (`NEENEE_*_DIR` since ADR-0014, the CI e2e job already
points four `XDG_*` vars at `runner.temp`), but assembling them correctly was
left to whoever remembered: five variables, one wrong and isolation silently
breaks, with no single name to point at and no guard when it is absent.

## Decision

1. **One selector, two entrances: `--home <dir>` and `NEENEE_HOME`.** The
   instance root is a single concept — every directory neenee touches moves
   under `<dir>/neenee/` (config, credentials, sessions, skills, logs, and
   the daemon's runtime files under `instance/`). The CLI flag is for
   interactive use; the environment variable is for process trees that
   cannot pass flags one invocation at a time — CI runners, `cargo test`,
   and the daemon a client auto-spawns. Both are documented as the same
   switch, and the flag wins when both are present.

2. **`Dirs::instance_dir()` is the single daemon-facing derivation.** All
   daemon runtime paths (`daemon.json`, `daemon.sock`, `daemon.lock`, legacy
   `serve/`) derive from it — never from `runtime_dir` directly — so every
   call site observes the same stack. Resolution: `--home`/`NEENEE_HOME`
   (`<home>/neenee/instance`) > `$XDG_RUNTIME_DIR/neenee` > data dir (the
   pre-0121 portable fallback).

3. **Precedence is specific-over-general.** Within each category: CLI flag >
   `NEENEE_<CATEGORY>_DIR` > instance root > `XDG_*` > native > `$HOME`
   fallback. A relative or empty instance root is ignored with a warning —
   a sandbox that only half-applies is worse than none, because it silently
   fails to isolate.

4. **`NEENEE_PORT` overrides the daemon's default TCP port** (below an
   explicit `--port`, above the well-known 9800). The daemon's discovery
   recovery path and `daemon status --diagnostic` honour it too. Isolation
   that redirects the socket but not the port leaves the two daemons
   fighting over 9800 with the silent ephemeral fallback masking it.

5. **Auto-spawned daemons inherit the sandbox by construction.**
   `client::spawn_daemon` and the detach path spawn
   `daemon start --fg` from `current_exe()` with the parent's environment,
   so a sandboxed client spawns a sandboxed daemon — which is also why the
   CLI's `--home` restates itself as `NEENEE_HOME` at startup: children
   inherit environments, not command lines. This is now a stated invariant
   (and covered by a regression test), not an accident.

6. **Tests default their whole process into a sandbox.** The runtime
   integration suites install `NEENEE_HOME` once, before any `paths::get()`
   resolution can cache a real-user view, replacing the hand-assembled
   five-variable sandboxes. Any future test that touches the resolver
   inherits isolation instead of having to remember it.

Nothing changes for the installed, unsandboxed path: with no instance root
and no `NEENEE_PORT`, resolution and port selection are byte-for-byte the
pre-0121 behaviour (the XDG runtime dir, then the data-dir fallback; port
9800).

## Alternatives considered

- **A separate `NEENEE_RUNTIME_DIR` for "runtime files only".** Rejected in
  review: two selectors for the same job. Every scenario it covered — a
  second user session, a dev sandbox, CI — is fully covered by the one
  instance root, and the extra layer cost a precedence row, a doc entry,
  and one more way to half-isolate. The one legitimate niche it named
  ("isolated durable data but daemon files on tmpfs") is `--home <tmpfs
  root>` plus the existing `$XDG_RUNTIME_DIR` when no root is given.

- **Only document the five existing variables.** Rejected: the failure mode
  of forgetting one is *silent partial isolation* — exactly the class of
  leak this repository has already paid for several times. A mechanism that
  needs perfect recall is not a mechanism.

- **Wrapper scripts (`nn`, `nn-test`) around the binary.** Rejected in
  review: the capability belongs in the binary, discoverable by `--help`,
  shell completion, and `ps`. A script layer is invisible to all three,
  adds PATH management, and forks the workflow into "documented CLI" and
  "what the wrapper actually does". The flag and the env var carry the
  whole mechanism; the docs carry the recipes.

- **Namespacing by binary path** (hash `current_exe()` into the runtime dir
  so a debug build never collides with an installed one). Rejected: it
  makes `target/debug/neenee` and a debug build copied elsewhere different
  instances, splits sessions across rebuilds (the path changes on every
  `cargo build` in some setups), and cannot be overridden by operators or
  CI. Instance identity should be explicit, not inferred from where the
  binary happens to live.

- **Version-gated discovery** (a dev daemon refuses to serve an installed
  client, or vice versa). Already partly true via `versions_compatible`
  (ADR-0100/0101): a rebuilt binary is refused with a mismatch error. But
  that is *collision detection*, not *collision prevention* — both daemons
  still contend for one lock, one socket file, one discovery record before
  any version check runs, and the check itself only helps when versions
  differ. Isolation removes the collision instead of reporting it.

- **Containers/VMs per dev session.** Rejected as the default: heavyweight,
  hostile to iterating on a TUI, and unnecessary — process-level env
  isolation covers every resource this daemon touches. Remains available to
  anyone who wants it; this ADR does not conflict with it.

- **A `[instance] name` config key** instead of env vars. Rejected: config
  lives *inside* the footprint being redirected, so the selector must be
  resolvable before config load. Env (or CLI) is the only layer that
  precedes it.

## Consequences

- A checkout can run its debug builds and full test suites on a machine with
  a live installed daemon, with zero shared state: no shared lock, socket,
  discovery record, port, sessions, config, credentials, or logs.

- `daemon status --diagnostic` reports the instance dir and port of *the
  instance the client resolves*, so an operator can see which sandbox a
  client is talking to (diagnosing "two daemons, one discovered" becomes a
  one-command check).

- One more CLI flag and env contract to document
  (`docs/reference/cli.md`, `docs/reference/paths.md`) and one more
  ADR-linked code path (`Dirs::instance_dir`) that daemon-facing code must
  use; a grep audit guards the invariant.

- The CI e2e job simplifies from four `XDG_*` exports to one `NEENEE_HOME`
  (plus an explicit port), demonstrating the canonical shape in the place
  contributors copy from.

- Migration is nil for users: no `--home` and no `NEENEE_HOME` means
  today's behaviour. Contributors opt in per-invocation with the flag, or
  export the variable for a whole shell.

## References

- ADR-0014 (XDG persistence architecture — the override stack this extends)
- ADR-0096 (unified session daemon — the runtime files this relocates)
- ADR-0100/0101 (daemon lifecycle/shutdown correctness — the lock and
  version-compat machinery that detects, but does not prevent, collisions)
- ADR-0105 (one port, two protocols — the fixed 9800 default `NEENEE_PORT`
  now overrides)
- `docs/reference/paths.md` (the updated precedence table)
- `docs/dev/dev-and-test-isolation.md` (the contributor workflow)
