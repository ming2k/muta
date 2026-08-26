# 0148. Doom-Guard Threshold Knob and Content-Aware Mutation Signatures

- **Status:** Accepted
- **Date:** 2026-08-27

## Context

ADR-0113 §5 flipped the pre-dispatch doom guard on by default and, in the
same batch, removed the `threshold`/`escalate_at`/`path_threshold` config
keys — hard-wiring the guard to block on the *first* same-signature repeat
(threshold 2). Field experience surfaced three false-positive classes that
made the guard's presence far heavier than its job requires:

1. **Sequential distinct edits to one file.** `edit_file`/`write_file`
   signatures keyed on `path` only, so the *second* edit to a file — normal
   multi-hunk work — collided with the first and was blocked. During the
   authoring of this very ADR the guard blocked a second, content-different
   `edit_file` to the same file mid-round: the exact false positive
   documented here, observed live.
2. **Test-edit-test iteration.** The window never clears on progress (by
   design: `A B A` is still a loop when `A` is `make test`), so
   `cargo test` → edit → `cargo test` in one round hit the first-repeat
   block on the second test run.
3. **Transient retries.** A flaky provider or tool failure followed by one
   honest retry of the identical call was blocked outright.

Meanwhile the variant-loop threat the strict posture defends against
(`sleep 1; make`, `sleep 2; make`, …) does not need a first-repeat block to
be caught: it needs the *third* occurrence at the latest, because a genuine
doom loop never stops at two.

## Decision

1. **Reintroduce `threshold` as a live `[master.doom_guard]` key** (default
   `3`, clamped to `>= 2` at use sites): a same-signature call is admitted
   until its in-window occurrence count — including the one about to run —
   reaches the threshold. Default `3` tolerates one same-signature re-run
   (transient retry, test-after-edit) and blocks the second repeat;
   `threshold = 2` restores the strict ADR-0113 first-repeat block. The
   legacy `escalate_at`/`path_threshold` keys remain ignored silently.
2. **Content-addressed mutation signatures**: `edit_file` signatures key on
   `path` plus a stable FNV-1a 64-bit hash of the payload
   (`old_string`/`new_string` for edits, `content` for writes);
   `write_file` likewise. Distinct edits to one file no longer collide
   (multi-hunk work proceeds), while an exact payload repeat — the true
   A→B→A thrash signal — still collides. `list_dir`/`read_image` stay
   path-keyed (no payload), and the no-payload fallback remains path-only.
3. **Block message wording** drops the occurrence-count phrasing ("times
   seen so far") because with a relaxed threshold the count is no longer
   fixed at trip time.

The detection remains pure signature bookkeeping with normalized locators
(ADR-0113 §5 unchanged), pre-dispatch, all watched tools, non-terminating;
the hard backstops (`hard_stop_turns`, `abort`, `Esc`) still cap. The knob
is wired through `muta config get/set master.doom_guard.threshold` and the
`[master.doom_guard]` TOML table.

## Alternatives considered

- **Progress-clears-window** (a different-tool call resets the window):
  rejected — an `A B A` interleave is still a doom loop when `A` is
  `bash run-tests`; the read-loop guard already handles the
  exploration-legitimate case.
- **Threshold escalation ladder** (the removed `escalate_at`): rejected
  again — two knobs for one trip point invite config drift; the single
  `threshold` plus the per-round signature mask covers the same ground.
- **Exclude `edit_file`/`write_file` from the watched set**: rejected —
   write-thrash loops are real; content-addressing keeps coverage while
   removing the false positive.
- **Default stays strict, users opt out via `threshold = 3`**: rejected —
  the false positives hit every normal workflow (multi-hunk edits,
  test-edit-test), not an edge case; a guard that must be tuned away to be
  usable has the wrong default.

## Consequences

- A same-signature call executes at most twice per round by default; the
  third is blocked pre-dispatch with the same steering note as before.
- Variant loops are capped at the third occurrence instead of the second —
  at most one extra iteration of a token-burning loop, bounded by the same
  hard backstops.
- Sequential distinct edits to one file no longer collide; an exact payload
  repeat still does.
- Existing `config.toml` files carrying `threshold` from pre-0113 versions
  now have that key honored again (previously ignored silently); a stale
  `threshold` value that was being ignored becomes live. Users who pinned
  `threshold = 2` keep the strict behavior they asked for.
- `DoomGuardConfig` gains a field; struct-literal constructors across
  crates use `..Default::default()` (none shipped user-visible APIs with
  exhaustive literals).

## References

- ADR-0113 §5 (default flip, signature normalization; the threshold
  removal this ADR partially reverses)
- ADR-0034 (range-aware read signatures — the per-locator normalization
  scheme extended here to mutation payloads)
- `crates/muta-agent/src/doom_guard.rs` (`check_ahead`, `doom_signature`,
  `stable_hash`), `crates/muta-contracts/src/doom_guard_config.rs`
