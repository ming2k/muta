# 0192. Inline History Recall Pointer Badge and the Escapable Recall State

- **Status:** Proposed
- **Date:** 2026-09-30
- **Builds on:** ADR-0174 (arrow edge hand-off — the recall gesture this decision makes legible), ADR-0173 (zero mode-indication tax — the constraint this badge is designed against), ADR-0172 (hints and dispatch share one semantic origin), ADR-0126 (queue-edit badge precedent on the same chrome row)

## Context

ADR-0174 handed the draft's arrow edges to inline history recall — the readline convention. The gesture is wired end to end: ↑/↓ move `App::history_index` over the current session's newest-first slice (`App::current_session_history`), the first ↑ stashes the live draft (text + staged attachments) into `history_draft_*`, and ↓ past the newest row restores it. But the state it creates is **invisible**:

1. **No indication that the buffer changed owner.** After a few ↑ presses the composer shows a history row and the user cannot tell whether they are editing the live draft or a read-only snapshot of a sent prompt. The confusion is consequential: an edit on a history row is temporary (the pointer model discards it when the pointer moves), so a user who "fixes a typo" in a recalled prompt and walks one more ↑ loses the fix silently. The Ctrl+R panel already announces itself (`[history search · draft saved]` on the same top chrome row, and the queue-edit badge before it) — the *most common* recall path is the only one with no declaration.
2. **No position sense.** `history_index` is a plain `Option<usize>` position in a slice whose length is recomputed per press. The user cannot tell how deep the walk has gone, how many rows exist, or how far they are from their draft.
3. **The state is inescapable by Esc.** `resolve_esc` (ADR-0172's session scheme) has arms for completion dismissal, step-focus clearing, and round interruption — none for the recall pointer. `App::cancel_history_recall` exists precisely for this (it restores the stashed draft) but had **zero callers**: dead code. The only exits from recall were walking ↓ to the end or sending the row — neither is the universal "get me back" gesture users reach for first.

The constraint that shaped the old refusal: ADR-0173's zero-mode-indication tax — the correct count of mode indicators for a modeless design is zero, and an announced state must pay for itself. That stance is preserved here, not violated: the recall pointer *is* a state that silently swaps the buffer under the user's keystrokes, exactly the class of state the tax exists to police. A history row that announces nothing but its existence (the content is already on screen) costs one clause on a row that already carries badges, and buys back content honesty.

## Decision

### 1. The pointer badge — a derived fact, not new state

`App::history_recall_badge() -> Option<(position, total, edited)>` derives the badge from existing state every render. No new App field is added:

- `position` is **1-based** (`history_index + 1`): the badge is prose for humans, not a slice index.
- `total` is `current_session_history().len()`, recomputed — the arrow paths already rebuild the slice per press, and a cached total would disagree with the live walk.
- `edited` is true when `input` no longer equals the loaded row's text — the pointer still addresses the row but the buffer has forked. The badge says `· edited` so the indicator never lies about what the buffer holds; the temporary-edit contract stays, but the user sees the fork *before* an ↑ discards it.
- `None` while `history_index` is `None`: **draft mode renders no badge.** The tax is paid only while the state exists.
- A `Some` pointer over an empty slice (possible only across a session-switch race) renders `None` — never a `0/0` badge addressing nothing.

The `· draft saved` clause is a presentation fact appended by the renderer (`!history_draft.is_empty()` plus staged attachments): the reassurance that ↓ past the newest row will restore the stashed draft.

### 2. Rendering — the top chrome row, priority and ladder

`ComposerHints` gains `history_recall: Option<(usize, usize, bool)>` and `recall_draft_saved: bool`; `render.rs` derives both while `&app` borrows are immutable. `ComposeTarget::HistoryRecall` is a new target (pointer outranks busy/slash — a content fact beats a surface state), with `HintState::Recall` in `session.rs` as its hint-row counterpart.

`top_chrome_row` priority: **overflow indicator > recall badge > extension badge.** The overflow indicator wins because clipped content is a spatial fact about the row's own role; the recall badge outranks the extension badge because content honesty always outranks surface naming when the two compete for one row. (In practice they rarely compete: the recall badge only renders while the Ctrl+R extension is closed.)

Width ladder (same degrade-then-simplify discipline as the overflow label):

| Width | Badge |
|---|---|
| ≥ 44 cols | `[history 3/17 · edited]` / `[history 3/17 · draft saved]` |
| ≥ 24 cols | `[history 3/17]` |
| ≥ pointer width | `[3/17]` — the pointer is the last element to go; it is the badge's entire point |

Styling inherits the existing badge channel: `theme.brand()` + DIM on the panel background, right-aligned with the same one-column air discipline as `[history search · draft saved]`.

### 3. The escapable recall state

- `InputContext.in_history_recall` (mirrors `App::history_index.is_some()`) reaches the resolver; `InputAction::CancelHistoryRecall` reaches the action layer; `App::cancel_history_recall` — previously dead — becomes the sole handler, promoted from dead code to the state's exit.
- `resolve_esc` gains the arm between completion dismissal and step-focus clearing: completion wins first (a popup is a strictly more superficial layer), the recall exit outranks step-focus clearing (the pointer state is the more urgent escape).
- The dispatch handler is a safe no-op when the pointer has already cleared, so a key race between the context snapshot and dispatch cannot clobber the restored draft.

### 4. The hint row declares the exit

`HintState::Recall` advertises `Esc draft` (nav) / `Enter send` (action). Per ADR-0172 the advertised chords and the resolver arms share one semantic origin, asserted by test: every chord the recall hint set shows resolves in the recall state.

## Alternatives considered

- **Persistent history-mode header, zsh-fish style** (`(reverse-i-search)`-class takeover). Rejected: the inline walk is a plain linear pointer, not a query-bearing search — the full apparatus pays the indication tax for information a one-clause badge carries. Ctrl+R remains the search surface and keeps its own heavier chrome.
- **Indicate via the text foreground** (tint recalled text). Rejected: color-over-content breaks the channel-separated discipline ADR-0174 §3 established — a single luminance/hue channel carries one meaning, and dyeing prose to name a mode collides with syntax/channel colors already on the text.
- **Reset `history_index` to `None` on any edit** (make the badge's `edited` clause unnecessary). Rejected: the pointer model deliberately preserves the position — ↓ after a discarded edit should walk forward from where you are, not jump to the draft; resetting would also orphan the draft stash (it is consumed by the ↓-past-newest path from a `Some` pointer).
- **Auto-cancel recall on Esc without a draft check.** Rejected in favor of the no-op guard: `cancel_history_recall` gates on `history_index.is_some()`, so the arm can never clear a live draft if state changed between snapshot and dispatch.
- **Do nothing** (the ADR-0174 status quo). Rejected: the invisible-state class is exactly what the indication tax polices; three usability findings (owner confusion, no position sense, inescapable state) all trace to it.

## Consequences

Positive:

- The recall pointer becomes legible: position, depth, edit-fork state, and draft safety in one clause on an existing row — zero layout churn, the badge mounts on chrome that already exists.
- The state is escapable by the universal chord; `cancel_history_recall` is promoted from dead code to the exit, and the hint row teaches it.
- Content honesty: the `edited` clause closes the silent-edit-loss gap identified in the pointer model's own docs.

Negative / neutral:

- One badge class now has three clause variants (`edited`, `draft saved`, bare) — the ladder and the priority rule must stay test-locked.
- `compose_target_for_extension` gains a parameter; the legacy adapter pins it to `false`, marking it for retirement with the adapter.
- Esc in recall state no longer falls through to step-focus clearing until the pointer is dismissed — a second Esc reaches the old arm.

Migration steps:

1. Derive the badge (`App::history_recall_badge`), thread it through `ComposerHints`, render on `top_chrome_row` (done).
2. Wire the Esc arm: `InputContext.in_history_recall`, `InputAction::CancelHistoryRecall`, dispatch → `cancel_history_recall` (done).
3. `ComposeTarget::HistoryRecall` + `HintState::Recall` hint set (done).
4. Update `docs/explanation/composer.md` (multi-slot pointer model) and `docs/reference/tui/input-box.md` (badge + hint row) (done).

## References

- ADR-0174 (arrow edge hand-off — the gesture made legible; its "no new indicator" reasoning is scoped here to *mode* indicators, while a content-state badge is the class the tax polices), ADR-0173 (indication tax), ADR-0172 (hints/dispatch single origin), ADR-0126 (queue badge precedent on the same row).
- `apps/tui/crates/mutx/src/app/history.rs` (badge derivation, recall cancel), `src/composer.rs` (`top_chrome_row`), `src/components/composer_hints.rs` (`HistoryRecall` target, `ComposerHints`), `src/session.rs` (`HintState::Recall`, `resolve_esc`), `src/input/mod.rs` (`in_history_recall`, `CancelHistoryRecall`), `src/event_loop/render.rs` (assembly), `src/event_loop/actions.rs` (dispatch).
