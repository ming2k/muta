# 0126. Queue affordances: Ctrl-row bindings, transcript-owned inserts, and a non-destructive queue pointer

- **Status:** Accepted
- **Date:** 2026-08-20

## Context

The queue subsystem (busy-Enter staging, mid-round inserts, the persistent
queue bar) had three UX failures:

1. **The queue family lived on the F-row** (`F2` expand, `F3` block/resume,
   `F4` insert). Fn-dispatch is OS/terminal policy, not app policy: terminals,
   window managers, and browser embedders freely reserve or remap F-keys, so
   on a large class of setups the binding silently never arrives — the user
   presses `F4` and nothing happens. The F-row is also unlabeled state: nothing
   on the keyboard says "F4 = insert into the running round". And the app's
   most time-sensitive gesture (steering a running turn) sat the farthest from
   home row.
2. **Inserts were invisible until admitted.** `F4` staged the message into the
   outbox as an `Inserting` item; the transcript only learned about it when the
   harness reported `UserInputInserted` at a safe turn boundary. During a long
   turn the user's message was only a one-line preview in the queue bar — it
   did not read as part of the conversation, and the whole `Inserting` state
   (excluded from dispatch, recall, delete, reorder) existed purely to paper
   over that split ownership.
3. **`↑` was destructive.** The only edit affordance for a queued message was
   `recall`: it *popped* the newest item into the composer. Editing an
   in-the-middle message meant pop-pop-pop, edit, re-queue ×3 — the queue is a
   list, but the gesture treated it as a stack.

## Decision

### 1. The queue family moves to the Ctrl row

- `Ctrl+Q` — open the Queue modal (was `F2`)
- `Ctrl+P` — block / resume the outbox (was `F3`; *pause*)
- `Ctrl+O` — insert into the running round (was `F4`; *open into the round*)

`F5` (the `/btw` asides list) stays on the F-row: it is a rarer, less
time-sensitive list surface with no clean free Ctrl slot. Ctrl chords are
distinct bytes under raw mode (`cfmakeraw` keeps ISIG/IXON off), survive
tmux/screen, and sit one row above the Enter the same gestures end with.

### 2. Inserts are transcript-owned entries

An insert (`Ctrl+O`) is appended to the transcript **at send time** as a
`Role::User` message with `DeliveryStatus::Queued` and a fresh `insert_id`
correlation. It renders as a normal user panel in the pending treatment
(`⏸ Queued` header, dimmer `user_panel_bg_queued` band) — visibly blocked on
the running turn — and it does **not** interrupt the running turn's entry,
which keeps streaming below it.

The `Inserting` outbox state is deleted. The outbox is now purely the
**next-round** queue (busy-Enter items and handed-back inserts), with two
states (`Waiting`, `Dispatching`).

Settlement, all keyed by `insert_id`:

| Harness event | Transcript entry | Outbox |
|---|---|---|
| `UserInputInserted` (admitted at a turn boundary) | flip to `Delivered`, stamp `origin = Insert` (`↳ insert`) | nothing (no item exists) |
| `UserInputUnavailable` (round ended first — **natural completion or an Esc Esc interrupt**; both are round terminations, and both are the insert's cue that its turn to be the *next* round's prompt has come) | flip to `HeldNextRound` (`⏸ Held for next round`) | stage a `Waiting` item with the same id and the entry's content |
| `NextRoundStarted` (the held item ships as a round prompt) | flip to `Delivered` | remove the item |

A held entry therefore never duplicates: the same message id renders from
staging through delivery, whether it was admitted mid-round or became the next
round's opener.

An insert accepts **no queue operations**: no recall, no edit, no delete, no
reorder, no cancel. It has already entered the conversation; the queue owns
only messages that have not.

### 3. A non-destructive queue pointer replaces `↑`-recall

`App::queue_pointer: Option<String>` (an outbox item id) forms a pointer model
that mirrors the history pointer:

- **`↑`** (queue non-empty) arms the pointer at the **newest** item and
  projects its content into the composer; further `↑` steps toward older
  items, clamped at the oldest. The first press stashes the draft
  (`queue_pointer_draft*`), exactly like the history pointer's stash.
- **`↓`** steps back toward newer items; past the newest it dissolves the
  pointer and restores the stashed draft.
- **Enter** commits the composer's content back into the pointed-at item
  **in place** — the queue's length and order are untouched. Editing `a` of
  `[a, b, c]` to `d` yields `[d, b, c]`: never `[b, c, d]` (a requeue) and
  never a duplicate.
- **Vanished target** (the item shipped, was deleted, or was recalled while
  the user was editing): the pointer is dissolved *without* restoring the
  stashed draft — the user's edit stays in the composer — and the send falls
  through to the ordinary path, treating the composer as a fresh message
  (queued if the session is busy). The experience never dead-ends on a race.

The pointer is held as an **id**, not an index, so dispatch/reorder/delete
cannot silently invalidate it. The pointer walks the queue *before* history:
`↑` enters the outbox first (the newer, more urgent surface) and only an
exhausted queue hands the key on to input history.

The queue modal's `Enter` re-edit keeps its destructive pull-to-composer
behavior — there, removing the item from the list is the point.

## Alternatives considered

- **Keep the F-keys, document them harder.** Rejected: the failure is
  environmental (key never arrives), not educational. No amount of copy fixes
  a binding the terminal eats.
- **Ctrl+G / Ctrl+S / Ctrl+F for the family.** Rejected: `Ctrl+G` is
  byte-collided with readline's abort-to-start-of-line without the Kitty
  protocol (the reason `F5` kept its slot), `Ctrl+S` collides with XON/XOFF
  flow control in terminals that re-enable it, and `Ctrl+F` is already
  readline `forward-char`.
- **Render pending inserts in the queue bar only, with a `steer›` badge**
  (the previous design). Rejected: it kept the split ownership (outbox item +
  later transcript push) that produced the `Inserting` state machine, the
  duplicate-push fallback, and the invisible-until-admitted UX.
- **Destructive recall with an insert-back.** Rejected for `↑`: pop-then-
  requeue changes queue order (`[a,b,c]` → edit `a` → `[b,c,a]`) and loses the
  item's slot identity, which is exactly what the pointer preserves.
- **Index-based pointer.** Rejected: every reorder/delete would silently
  retarget it. The id survives all three and makes "vanished" detectable.

## Consequences

- The queue bar/modal legend, Help modal, and docs all read
  `Ctrl+O insert · Ctrl+P block · Ctrl+Q expand` from the single keymap
  registry, so the vocabulary cannot drift.
- `DeliveryStatus` gains `HeldNextRound`; the `⏸ Queued` render path that was
  previously production-dead now carries both pending states, and the
  live-path insert renders `↳ insert` provenance exactly like a restored one.
- `requeue_dispatch` gains a content-bearing arm (staging a handed-back
  insert's text into the outbox); images/pastes for a handed-back insert are
  currently dropped (the held entry keeps them in the transcript; the
  re-queue uses its raw text). Acceptable: the common hand-back is
  text-steering.
- `InsertUserInput` now ships the composer's staged images (the old path sent
  `Vec::new()` and silently dropped them).
- The F2/F3/F4 bindings are gone; users must relearn three chords. Mitigated
  by the mnemonics (Q/P/O), the bar legend, and Help sharing the registry.
- Sessions persisted mid-insert (before settlement) restore as ordinary
  `UserSteer` messages — the pending states are presentation-only and live in
  the TUI, never in the durable store.

## References

- `docs/reference/tui/queue-bar.md`, `docs/reference/tui/input-box.md`
- `crates/neenee-tui/src/keymap.rs` (the registry), `crates/neenee-tui/src/app.rs`
  (`queue_pointer` family), `crates/neenee-tui/src/model/document.rs`
  (`DeliveryStatus`), `crates/neenee-tui/src/event_loop/actions/commands.rs`
  (insert staging + in-place commit), `crates/neenee-tui/src/lib.rs`
  (settlement).
- ADR-0092 (guaranteed activity resolution — the insert acks stay
  event-driven), ADR-0103 (`F5` asides kept).
