# 0142. Additional workspace roots: the cross-project escape hatch

- **Status:** Partially superseded by ADR-0147 (root admission is user-owned global policy)
- **Date:** 2026-06-30

## Context

ADR-0096 made sessions daemon-scoped and ADR-0140 made the project root the
single filesystem authority: every path-taking tool resolves against the
session's canonical `WorkspaceRoot`, `WorkspaceFsProvider` fails closed on any
path outside it, and the bash tool's `Workspace` isolation admits only the
workspace plus a minimal system runtime into the sandbox. This is a safe
default — but it has no **escape hatch**. Real workflows routinely span
repositories:

- a frontend in `apps/web` talking to a backend in a sibling repo;
- a library consumed from a local path override while its consumer is the
  open project;
- verifying a change against a checked-out copy of another project;
- referencing a shared design/assets directory next to the project.

Today every one of those forces one of two blunt tools: `/trust` to move the
whole session to host execution (no containment at all), or `--project` to
re-root the session elsewhere (abandons the original root). Neither is
"cross-project"; both are "instead-of-project". Other agents solve this with
an `additionalDirectories` mechanism — a set of extra admitted directories
alongside the primary root.

## Decision

1. **Config surface.** A project may declare additional admitted roots in
   `.muta/config.toml`:

   ```toml
   [workspace]
   additional_roots = ["../backend", "~/projects/design-kit"]
   ```

   Paths resolve **relative to the project root** (never the process cwd),
   `~` expands to the user's home, and each entry canonicalizes at load. The
   primary root is implicitly admitted and must not be repeated. Entries that
   do not exist, are not directories, or nest inside the workspace are
   rejected with a precise error naming the offending entry.

2. **Contract surface.** `WorkspaceRoots` (plural) becomes the multi-root
   authority on `ToolContext`: a primary root (relative resolution, shells'
   cwd) plus canonicalized additional roots. `WorkspaceRoot` remains for
   single-root consumers. A roots type answers three questions — `contains`
   (admission), `primary` (resolution), and `iter` (surfacing to the model).

3. **Enforcement.** `WorkspaceFsProvider` admits a path when it falls under
   *any* root. The bash sandbox receives every root as read-write bind
   mounts; the primary root stays the cwd. Isolation is otherwise unchanged
   (fail-closed, minimal system runtime, network disabled).

4. **Trust is per-root and content-free.** Additional roots extend *where*
   the session may operate, not *what* it may do. The extension/MCP
   content-bound trust (ADR-0140) is untouched: an attacker who can write
   `.muta/config.toml` already controls project hooks and MCP servers, so the
   additional-roots table grants no capability that table does not. One
   deliberate narrowing: **an additional root is never considered when
   deciding what counts as "the project" for skill/extension discovery** —
   those stay bound to the primary root.

5. **Model visibility.** The system prompt lists additional roots so the
   model knows cross-project paths are admissible; tool errors name the
   offending path and the admitted root set.

## Consequences

- Cross-project联调 works with the sandbox intact: no `/trust`, no re-rooting.
- The fail-closed property survives: an unset or malformed `[workspace]`
  table changes nothing.
- Future work (not here): a `/roots` runtime command to add/drop roots
  mid-session, and per-root access modes (read-only additional roots).
