# 0079. Re-merge `neenee-tui-view` into the application binary

- **Status:** Accepted
- **Date:** 2026-07-23
- **Supersedes:** ADR-0045 (the extraction this ADR reverses)

## Context

ADR-0045 (2026-07-10) carved `neenee-tui-view` out of the binary's `tui/`
module tree to fix three pains: widgets testable only through the whole
`App`, a shell→view boundary the compiler could not enforce (two read-only
crossings had crept in under convention), and an 18k-line fat shell module.

Thirteen days later the balance looked different:

1. **Single consumer.** Only the `neenee` binary depended on
   `neenee-tui-view` — no second frontend, no out-of-tree embedding. That is
   the first leg of the consolidation signature ADR-0074 used
   (single consumer + lockstep change + no distinct earning responsibility).
2. **Near-lockstep change.** In the 240 commits scanned, the view crate was
   touched in 48, and 44 of those (92%) also touched the binary.
3. **The boundary was invisible in daily use anyway.** The shell kept a
   `pub(crate) use neenee_tui_view::{fuzzy, model, providers, view, …}`
   re-export shim (`crates/neenee/src/tui/mod.rs:28-36` at the time) so call
   sites still addressed the view as `crate::tui::*`; the crate line existed
   for the compiler, not for the reader.
4. **Workspace cost.** The workspace carried 13 crates; the view crate was
   the largest of them (29.5k lines) and its extraction had not stopped the
   shell and view from evolving as one module tree.

The retained benefit of the split — the compiler-enforced one-way seam —
was real but narrow, and the pre-split evidence (two crossings under
convention) cuts both ways: it shows the boundary needs care, not that a
crate line is the only way to hold it. The user decided to trade the
enforcement back for one less crate.

## Decision

Move all of `crates/neenee-tui-view/src/` back into the binary as modules of
`crates/neenee-cli/src/tui/` (the binary is `neenee-cli` after ADR-0080) and
delete the crate:

- **Flat merge.** The view's 16 root files + 6 module dirs
  (`model`, `components`, `disclosure`, `layout`, `overlays`, `tools`) land
  directly in `src/tui/`. The only filename collision was `completion.rs`;
  the view's type definitions (`Completion` / `CompletionKind` /
  `CompletionItemKind`) were fused into the shell's existing file, and the
  `pub use neenee_tui_view::completion::{…}` re-export deleted — call sites
  keep using `crate::tui::completion::*` unchanged.
- **The shim is deleted, not replaced.** `tui/mod.rs` declares the merged
  modules directly. One exception to "no glob": the old crate root's
  `pub(crate) use view::*;` is kept in `tui/mod.rs` because four bare-path
  call sites (`layout/mod.rs:41`, `disclosure/mod.rs:67`, `tools/mod.rs:34`,
  `message_body.rs:28`) relied on it.
- **Visibility restoration.** `pub(super)` in the moved root files (103
  occurrences) became `pub(crate)`: inside the old crate those items were
  effectively crate-visible; after the move `pub(super)` would have narrowed
  them to `crate::tui` and broken `view.rs`'s re-exports (E0364).
- **Snapshots move with their tests.** The 15
  `neenee_tui_view__snapshot_tests__*.snap` files were renamed
  `neenee__tui__snapshot_tests__*` and now share
  `crates/neenee-cli/src/tui/snapshots/` with the 4 existing
  `question_model` snaps. Recorded gotcha: insta's filename prefix derives
  from the **test-target crate name** (the `[[bin]]` name `neenee`), not
  from the package name.
- **Dead surface.** 10 `#[allow(dead_code)]` annotations mark items that
  were crate-public API consumed by nobody (test-only helpers and four
  genuinely orphaned items: `Block::inline`, `McpRow`, `toast()`,
  `HintBarView.messages`). Nothing was deleted; a future cleanup can prune
  them deliberately.

`neenee-tui-engine` is untouched; the binary is now its sole consumer.

## Alternatives considered

- **Keep the crate (status quo).** Rejected by the user: single consumer,
  92% co-change, and the shim made the boundary invisible to call sites, so
  the crate cost a manifest and a second compile unit without earning either.
- **Fold the widgets into `neenee-tui-engine` instead.** Rejected (unchanged
  from ADR-0045): the engine must know nothing about neenee; the widgets
  render `neenee-core` domain types.
- **Nest under `src/tui/view/` as one sub-tree.** Rejected: every call site
  path (`crate::tui::fuzzy`, `crate::tui::model`, … via the shim) would have
  needed rewriting; the flat merge preserves them.

## Consequences

- **Positive.** One less crate and manifest; the `tui/` tree reads as the
  single module it has effectively been since the shim was added.
  `cargo build` no longer compiles the view as a separate unit the shell
  then re-links.
- **Positive.** The re-export shim and its maintenance are gone; the
  `neenee-cli` Cargo.toml loses a path dep.
- **Negative (accepted).** The compiler no longer enforces shell → view.
  The seam is convention again, held by: the `SessionSource`-style borrowed
  view structs (`TranscriptView<'a>` &c., unchanged), code review, and the
  module-level doc in `tui/mod.rs` restating the three-layer rule (engine /
  view modules / shell). If crossings creep in again, re-extracting is a
  mechanical reverse of this ADR.
- **Negative (accepted).** `cargo test -p neenee-cli` compiles the whole
  binary for widget iteration (566 tests at merge time vs. the view crate's
  standalone 181+). Measured impact at this codebase size is seconds; noted
  for the record.
- **Neutral.** Workspace crate count is unchanged net of ADR-0081 (12 after
  this merge, 13 again with `neenee-server`) — the removed crate was a
  non-earning boundary, the added one is a new capability.
- **Verification at merge time.** `cargo test -p neenee-cli`: 566 passed
  (all 15 file-backed snapshots + 4 question_model films in place, no
  `.snap.new` strays); `cargo test --workspace`: 1450 passed;
  `cargo clippy --workspace --all-targets -- -D warnings`: clean.

## References

- [ADR-0045](0045-extract-neenee-tui-view.md) — the extraction this ADR
  reverses (status now Superseded).
- [ADR-0074](0074-consolidate-llm-client-crate.md) — the consolidation
  signature (single consumer + lockstep) applied here.
- [ADR-0080](0080-rename-neenee-to-neenee-cli.md) — the binary's package
  rename; the merge landed after it, so paths above read `neenee-cli`.
- [ADR-0038](0038-in-house-grid-diff-rendering-engine.md) — the engine
  crate, which stays put.
