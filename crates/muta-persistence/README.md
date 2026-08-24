# muta-persistence

Durable state and configuration for the muta agent stack.

`muta-contracts` holds the pure domain (types & traits), zero I/O. This crate
sits one layer above it: the durable state and configuration a frontend needs
to actually run a session:

- config loading + whole-file validation (`config.rs`, `config_check.rs`) and
  path resolution (`paths.rs`, the single point of truth per ADR-0014);
- the **event-sourced session store** — which also carries the `/schedule`
  cron calendar as session-scoped state — blob storage (with mark-sweep
  garbage collection), the embedding index, and usage telemetry
  (day-partitioned, retention-bounded).

This is the **local agent** persistence layer. It assumes a single-user
workstation: paths resolve through XDG `ProjectDirs`, sessions are keyed by
project root, and cross-process writes to shared files serialise through
companion `.lock` flocks (ADR-0018).

Frontends depend on `muta-contracts` + `muta-persistence` and add their own
presentation layer; they never reach into a sibling frontend's crate. See
ADR-0005 for the layering rationale.
