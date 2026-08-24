# 0080. Rename `neenee` → `neenee-cli` (package); the command stays `neenee`

- **Status:** Superseded by ADR-0136
- **Date:** 2026-07-23
- **Revises:** ADR-0075 (which renamed `neenee-code` → `neenee` earlier the
  same day, when the application layer was a single binary)

## Context

ADR-0075 renamed the sole application crate `neenee-code` → `neenee` on the
grounds that a single product needs no role suffix. Days later, ADR-0081
introduces a **second** application binary — `neenee-server`, a headless
session host. With two application-layer binaries the "sole binary gets the
bare package name" premise no longer holds: one of them would be named for
its role (`neenee-server`) while the other carried the bare product name,
which reads as if the TUI were the product itself and the server an add-on.

The cargo/git/nvim convention from ADR-0004 still applies, split across two
names: the **package** gets the role suffix (`neenee-cli`), the
**user-facing command** keeps the bare name (`neenee`) because it is the
primary tool.

## Decision

- `crates/neenee/` → `crates/neenee-cli/`; `[package] name = "neenee-cli"`.
- `[[bin]] name = "neenee"` is unchanged — every user invocation, alias,
  `install.sh` (`BIN_NAME="neenee"`), release tarball name, and shell
  completion keeps working. `cargo -p neenee-cli` selects the package.
- Root `Cargo.toml` `default-members` → `["crates/neenee-cli"]`.
- Mechanical sweeps: `crates/neenee/` path references in `install.sh`,
  crate README, showcase demo payloads, `docs/` path references; `-p neenee`
  selectors → `-p neenee-cli`. `.github/workflows/` needed no changes
  (release builds `--bin neenee`).

Recorded empirical correction to a prediction made during planning: insta's
snapshot filename prefix derives from the **test-target crate name**, which
for a binary crate is the `[[bin]]` name — still `neenee`. The four
`neenee__tui__question_model__tests__*.snap` files therefore did **not**
need renaming (only their `source:` headers moved); renaming them by
package name produces `.snap.new` strays and test failures.

## Alternatives considered

- **Keep package `neenee` beside `neenee-server`.** Rejected: two
  application crates, only one of them role-named, revives the ADR-0005
  complaint about names that describe mechanism ("app") instead of purpose.
- **Rename the binary too (`neenee-cli` as the command).** Rejected: breaks
  every existing user invocation, the installer, and release artifacts, for
  symmetry nobody interacts with (users run commands, not package names).

## Consequences

- **Positive.** The two application crates read as peers
  (`neenee-cli` / `neenee-server`); the command surface is untouched.
- **Neutral.** Pure workspace-internal rename; the crates are not
  published. `Cargo.lock` reconciles itself.
- **Verification.** `cargo build/test/clippy -p neenee-cli` green (312
  tests at the time), `target/debug/neenee` still the output path.

## References

- [ADR-0075](0075-rename-neenee-code-to-neenee.md) — the rename this ADR
  revises (its single-binary premise no longer holds).
- [ADR-0081](0081-neenee-server-and-attach-model.md) — the second binary
  that motivated this rename.
- [ADR-0004](0004-six-crate-topology.md) — the bare-name-for-primary-tool
  convention.
