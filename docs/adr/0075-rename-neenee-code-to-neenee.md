# 0075. Rename `neenee-code` → `neenee` (single-product rename)

- **Status:** Accepted (the single-binary premise was reversed by ADR-0080:
  the package is `neenee-cli` again; the `[[bin]] name = "neenee"` choice
  this ADR made still stands)
- **Date:** 2026-07-23
- **Reverses:** the application-layer rename sub-decision of ADR-0035 (crate
  `neenee-cli` → `neenee-code`, binary `neenee` → `neenee-code`)

## Context

ADR-0035 renamed the coding application crate `neenee-cli` → `neenee-code` and
the binary `neenee` → `neenee-code` for one specific reason: a **second**
domain application (`neenee-quant`) was being added, and a bare `neenee` binary
would have *implicitly* meant "the coding one" — an invisible coupling between
a generic name and one domain. With two domain commands, both should carry
their domain so neither is privileged by ambiguity. ADR-0035 recorded this
explicitly under "Alternatives considered":

> a bare `neenee` would *implicitly* mean "the coding one", an invisible
> coupling between a generic name and one domain.

ADR-0073 (same day as this decision) removed the editor and quant products.
The repository now ships a **single** product, the coding agent. ADR-0073
flattened the workspace layout but deliberately left package names unchanged,
deferring the rename question to a separate decision:

> Package names are unchanged ... A second product can be reintroduced later;
> doing so would warrant a new layout decision rather than reverting this one.

That deferred decision is this ADR. The force that earned `-code` its keep — a
sibling domain binary to disambiguate from — no longer exists. "neenee" is the
final product name (it matches the repository, the GitHub release tarball
prefix `neenee-<version>-<target>`, and the workspace package family
`neenee-*`). ADR-0005's original sub-decision ("the binary stays `neenee`") was
correct under the single-product assumption; ADR-0035 revised it for a
multi-product world that has since been rolled back.

## Decision

Rename the sole application crate and its binary back to the bare product name:

| Old | New | Rationale |
|-----|-----|-----------|
| `neenee-code` (crate) | **`neenee`** | With one product the domain suffix has no sibling to disambiguate from. The bare name matches the product, the repository, and the release artefact prefix. |
| `neenee-code` (binary) | **`neenee`** | `[[bin]] name = "neenee"` in `crates/neenee/Cargo.toml`. Shorter invocation; consistent with the product name users already know. |
| `crates/neenee-code/` (dir) | **`crates/neenee/`** | Directory matches package name, per the workspace's "package names match their directory names" convention. `git mv` preserves history. |

This reverses the rename half of ADR-0035. The other half of ADR-0035 — the
`neenee-quant` application — was already dismantled by ADR-0073, so ADR-0035 is
fully superseded (both its decisions are no longer in effect).

Nothing else about the topology changes. The strict-DAG property from
ADR-0005, the flat `crates/` layout from ADR-0073, the application's
responsibilities, and its dependency set are all unchanged. This is a pure
rename: same binary, same behavior, shorter name.

## Alternatives considered

- **Keep `neenee-code`.** Rejected: the `-code` suffix existed solely to
  disambiguate the coding binary from `neenee-quant`. With quant and editor
  gone, the suffix names a distinction that no longer exists. Keeping it would
  preserve a multi-product vocabulary in a single-product repository, and leave
  the public command (`neenee-code`) longer than the product name (`neenee`)
  for no remaining reason.

- **Rename the crate but keep the `neenee-code` binary.** Rejected: a
  crate/binary name split (`neenee` crate producing a `neenee-code` binary) is
  the worst of both — it hides which command a user runs behind a different
  package name, and breaks the "package names match directory names" rule.
  ADR-0035 deliberately aligned crate and binary names; this ADR keeps them
  aligned.

- **Defer until a second product returns.** Rejected: names shape how users
  invoke the product every day. Shipping the 1.0 command as `neenee-code` on
  the speculation that a sibling might reappear would impose the cost now for a
  benefit that may never materialize. If a second product returns, ADR-0073
  already says a new layout decision is warranted — a rename then would be no
  harder than now.

- **Prefix the binary with a short alias (`nee`).** Rejected: inventing a new
  shorthand diverges from the product name and the repository name. The bare
  product name is already short and unambiguous.

## Consequences

- **Positive.** The command, the product, the repository, and the release
  tarball prefix all agree on one name: `neenee`. New users do not need to
  learn why the binary is `neenee-code` when the product is `neenee`.

- **Positive.** Invocation is shorter: `neenee` over `neenee-code`.

- **Positive.** The workspace reads symmetrically: every member is
  `neenee-<layer>`, and the application that ties them together is the bare
  `neenee` — the root the family is named after.

- **Negative (one-time, breaking).** Every existing `neenee-code` invocation
  becomes `neenee`. Users must reinstall or symlink. Recorded under
  `[Unreleased]` → `Changed` in `CHANGELOG.md`. The install script, release
  workflow, shell completion, and any user PATH entries move from `neenee-code`
  to `neenee`.

- **Neutral.** A second product reintroduced later would warrant either
  re-adding a domain suffix to both binaries or adopting a different
  disambiguation strategy. That is a future ADR's problem, not a constraint on
  the single-product present.

## Migration mechanics

| What | Files | Notes |
|------|-------|-------|
| `git mv` directory | `crates/neenee-code/` → `crates/neenee/` | history preserved |
| package + `[[bin]]` name | `crates/neenee/Cargo.toml` | `neenee-code` → `neenee` |
| workspace default member | root `Cargo.toml` | `default-members = ["crates/neenee"]` |
| insta snapshot files | `crates/neenee/src/tui/snapshots/neenee_code__*.snap` → `neenee__*.snap` | insta derives the prefix from the crate name |
| insta snapshot `source:` metadata | 5 `.snap` files | path updated; one stale pre-split `source:` corrected to its real tui-view path |
| install script | `install.sh` | `BIN_NAME`, comments, messages |
| release workflow | `.github/workflows/release.yml` | `--bin`, `cp` paths |
| lockfile | `Cargo.lock` | package name; reconciled by `cargo build` |
| doc comments + READMEs | across crates | `neenee_code` / `neenee-code` → `neenee` |
| living docs | `docs/dev/`, `docs/reference/`, `docs/explanation/`, `docs/how-to/`, `docs/reference/glossary.md` | mechanical rename |
| ADR status + index | ADR-0005, ADR-0035, `docs/adr/index.md` | status lines + index only; ADR decision bodies left intact |

ADR decision bodies (0035, 0037, 0039, 0045, 0050, 0053, 0054, …) still
contain `neenee-code` path references. Per ADR workflow they are immutable
historical records and are left unchanged; the glossary and this ADR carry the
current truth.

The build is clean and all 312 tests in the renamed crate pass, including the
four insta snapshot tests whose files were renamed.

## References

- [ADR-0005](0005-strict-layering-and-renames.md) — the original "binary stays
  `neenee`" sub-decision this restores.
- [ADR-0035](0035-application-layer-split.md) — the rename being reversed
  (superseded).
- [ADR-0073](0073-flat-coding-focused-workspace.md) — removed the editor and
  quant products, leaving `neenee-code` as the sole application and making this
  rename possible.
- [Workspace layout](../dev/workspace-layout.md)
