# 0143. Filesystem search tools have one job each

- **Status:** Accepted
- **Date:** 2026-08-26

## Context

The model-visible filesystem surface exposed `glob`, `find`, `list_dir`, and
`grep`. The first three overlapped on file discovery while accepting different
schemas and pattern dialects; `list_dir` also switched between shallow listing,
recursive traversal, and glob filtering. Ignore rules, ordering, limits, and
errors consequently varied by tool.

The overlap encouraged calls such as a brace-packed `glob` containing `**`.
The underlying Rust `glob` parser rejected that expression even though the
model reasonably treated it as a list of alternatives. Documentation alone
cannot make an ambiguous tool boundary reliable.

Content search is a different operation: it needs a regex or literal query,
line-oriented output, context lines, and file selection. Combining it with
path discovery would replace several overlapping tools with one overloaded
tool whose mode is inferred from optional fields.

## Decision

Expose exactly three filesystem navigation and search tools to the model:

1. `list_dir` lists immediate children. It never recurses or filters.
2. `find_files` recursively discovers files. Its required `patterns` array
   represents OR explicitly; `path`, `exclude`, `max_depth`, and `limit`
   constrain traversal.
3. `search_text` searches file contents. Its required `query` is a regular
   expression unless `literal` is true; `include` and `exclude` select files.

Retire `glob`, `find`, and `grep` without model-visible aliases. Tool names and
required field names state the operation and input language: plural
`patterns` for path globs, singular `query` for content search.

Use one shared implementation layer for path admission, glob validation,
project ignore rules, hidden-file behavior, deterministic traversal, hard
directory exclusions, and result limits. `find_files` uses the Rust `ignore`
walker. `search_text` runs in-process with Rust's `regex` engine and the same
walker; it never depends on an `rg` executable.

All three tools use closed JSON schemas, reject unknown fields, resolve
relative paths from the primary workspace, and admit absolute paths only
inside configured workspace roots. They declare read/search tree accesses so
the scheduler serializes them against overlapping writes.

## Alternatives considered

- **One `search` tool with discovery and content modes.** Rejected because its
  meaning would depend on mutually exclusive optional fields and produce two
  incompatible result shapes. This reduces tool count but increases call
  ambiguity.
- **Keep all four tools and improve their descriptions.** Rejected because the
  schemas would still overlap and pattern behavior would still differ.
- **Expose ripgrep directly for both operations.** Rejected because a process
  command is not a stable tool contract, may be unavailable, and does not by
  itself enforce Muta's workspace and output policies.
- **Keep retired aliases for compatibility.** Rejected under ADR-0135: aliases
  preserve stale vocabulary in policies, prompts, presenters, and tests.

## Consequences

- A caller expresses multiple filename alternatives as separate array items,
  so brace syntax is unnecessary and the reported recursive-wildcard failure
  class is removed from normal calls.
- Models have one obvious tool for shallow browsing, file discovery, and text
  search respectively.
- Project ignore behavior and caps are consistent across discovery and content
  search.
- Stored prompts or external policies naming `glob`, `find`, or `grep` must
  migrate to `find_files` or `search_text`. `list_dir` callers that relied on
  recursion or filtering must migrate to `find_files`.
- The public surface intentionally keeps two search tools because path matching
  and content matching use different input languages and return types.

## References

- [ADR-0135: Retirement deletes](0135-retirement-deletes-no-teaching-errors.md)
- [ADR-0140: Workspace authority](0140-workspace-authority-and-content-bound-extension-trust.md)
- [ADR-0142: Additional workspace roots](0142-additional-workspace-roots.md)
- [Filesystem tools reference](../reference/tools/filesystem.md)
