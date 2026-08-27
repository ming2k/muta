# 0152. Implicit temporary-directory admission for the file-tool plane

- **Status:** Accepted
- **Date:** 2026-08-28
- **Revises:** ADR-0142 (widens the default admitted set); orthogonal to ADR-0147's trust planes

## Context

The spatial workspace boundary admitted exactly the primary workspace root
plus user-configured additional roots (ADR-0142). Any absolute path outside
that set — including `/tmp` — was denied to the built-in file tools.

That default collided with ordinary agent workflows that stage data in the
platform temp directory: output spill files, build probes, download staging,
cross-session scratch. The agent could *create* such files only through the
bash tool (whose Linux sandbox mounts a fresh tmpfs on `/tmp`, and whose macOS
variant runs unsandboxed by default), then could not read its own spill file
back with `read_text`. Every affected workflow needed the operator to manually
configure `workspace.additional_roots = ["/tmp"]` — a step that is easy to
forget, per-project, and whose omission surfaced as confusing mid-task
`PermissionDenied` errors.

A related concern came from multi-root setups on macOS: `std::env::temp_dir()`
returns `$TMPDIR` (`/var/folders/…/T`), while the same directory canonically
resolves to `/private/var/…/T` (and `/tmp` to `/private/tmp`). An admission
set spelling only one of the two would still deny the other, depending on
whether a caller passed the well-known name or a resolved path.

## Decision

**The file-tool containment plane implicitly admits the platform temporary
directory.** The admitted set for built-in file tools becomes:

1. the canonical primary workspace root,
2. user-configured additional roots (ADR-0142, trust-gated for project
   declarations per ADR-0147),
3. the implicit temp roots: `std::env::temp_dir()`, `/tmp`, and the
   canonicalized spelling of each (`/private/tmp` on macOS), deduplicated.

Temp admission is **implicit and unconditional**: it is not a trust domain,
not persisted, not project-scoped, and not revocable through `/trust`. The
temp directory is shared infrastructure by definition — the operator has no
meaningful choice to make about it, and gating it behind a prompt would just
re-create the friction this ADR removes.

### Where the check lives

Temp admission is added at the three containment planes that evaluate
absolute file paths, and nowhere else:

- `ExecutionEnvironment::resolve_path` trait default
  (`muta-contracts::execution`) — the jail middleware's pre-execute check.
- `WorkspaceFsProvider::confined` (`muta-agent::execution::local`) — the
  physical fs provider every file tool flows through.
- `resolve_search_root` (`muta-agent::tools::file_search`) — the walk-root
  scope for `find_files` / `search_text` / `list_dir`.

It is deliberately **not** added to `additional_roots()`, the handle the
bash-sandbox assembler consumes. The Linux workspace sandbox mounts a fresh
tmpfs on `/tmp` (so a sandboxed command's temp writes never touch the host);
binding the *host's* `/tmp` into the container through the additional-roots
path would defeat that isolation. Keeping temp admission out of that handle
preserves the sandbox's minimal-mount contract — sandboxed commands keep
their private tmpfs, while file tools gain host-temp readability.

### Matching both spellings

`temp_roots()` resolves raw and canonical spellings of both `env::temp_dir()`
and `/tmp`. The check itself stays purely lexical (`starts_with` over
`lexical_normalize`) with no per-call syscalls, mirroring the additional-root
checks; the only fs call is the one-time canonicalization inside
`temp_roots()`.

### Consequences for tests

Existing denial tests that used `tempfile::tempdir()` as the "outside"
witness no longer exercise a denial — their fixtures now sit inside the
admitted set. They anchor their outside fixtures under the gitignored
`target/test-scratch` directory instead (`workspace_tests_outside_scratch`).

## Consequences

- Scratch workflows (spill files, staging, probes) work out of the box on
  Linux and macOS, with no configuration and no prompts.
- `/tmp` is now writable through file tools wherever the session runs — the
  operating system already treats it as world-writable scratch, so the
  marginal exposure is small; secrets in workspaces outside the admitted set
  remain unreachable.
- The bash-sandbox tmpfs contract is untouched: sandboxed commands still
  cannot see host temp files through `/tmp`.
- Windows inherits the same rule through `env::temp_dir()` (`%TEMP%`), which
  is user-scoped, so the cross-platform semantics are uniform.

## Alternatives considered

- **Prompt-gated trust domain** (`/trust tmp`): rejected — adds a decision
  the operator has no stake in; a shared scratch directory is not a
  cross-project trust elevation.
- **Config opt-in** (`workspace.admit_tmp = true` default-off): rejected —
  per-project ceremony for a universal workflow; the default would keep
  producing the confusing denials this ADR exists to remove.
- **Only `/tmp`, not `env::temp_dir()`**: rejected — macOS's `TMPDIR`
  (`/var/folders/…`) is where `tempfile` and most tools actually write; a
  bare `/tmp` rule would leave the common case broken on macOS.
