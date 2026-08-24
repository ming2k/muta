# Input box

The live editable prompt at the bottom of the frame.

## Appearance

```text
  ┃                                   ← top padding (full panel-bg row)
  ┃ type here…                        ← text row(s)
  ┃                                   ← bottom padding (full panel-bg row)
```

| Attribute | Value |
|-----------|-------|
| Background | `input_bg_active` (26, 28, 27) when the box owns the keyboard; `input_bg_inactive` (16, 17, 17) while a transcript step has focus (the pair is independent of every other surface token) |
| Left/right margin | 2 cols of `app_bg` |
| Accent bar | `┃` in `accent` (Build mode) or Plan-mode blue |
| Text color | `text` (brighter than sent messages) |
| Text indent | 4 cols (2 margin + `┃` + 1 leading space) |
| Top/bottom padding | Full panel-bg rows (no half-block glyphs — a cell can only carry one bg color, so a solid row is identical across terminals) |

## Height growth

The box grows with wrapped content, capped at half the terminal height so the
transcript history always stays visible. The layout reserves space based on
`wrap_text(input, text_width).len()`.

## Caret

Blinking terminal caret positioned on the active wrapped line. Clamped to the
visible inner area when the input is very long.

## Selection relay

While a selection covers the composer's text the block caret is hidden — but
its position is *remembered*: the selection's **head**, the point where the
mouse button was released. The next non-typing key relays from that hidden
position and breaks the selection, so a drag is never "lost state":

| Key | Behaviour over an active input selection |
|-----|------------------------------------------|
| `←` / `→` | Adopt the caret at the head edge, then step one char (word with Ctrl/Alt) in the pressed direction |
| `↑` / `↓` | Restore the caret at the head edge and consume the press; line-walking / history recall resume from there |
| `Home` / `End` | Adopt the tail / head edge of the selection |
| `Backspace` / `Del` / `Ctrl+W` / `Ctrl+U` / `Ctrl+K` / `Alt+D` | Replace the selection — the whole selected text goes in one stroke |

The relay only engages when the composer owns the caret (no modal, no focused
transcript step): while a step holds keyboard focus, arrows keep their
step-navigation meaning. A click inside the box also breaks the selection and
parks the caret at the clicked character.

## Delete key

`Del` is the forward delete: it removes the character *after* the caret and
never moves it. It works everywhere the composer line is edited (the main
prompt, the history/model filter fields, the provider editor, the key editor,
and the `/host` inline prompt), respects whole grapheme clusters (a CJK glyph
or emoji vanishes as one unit), and is chip-aware — a `Del` landing on the
`[` of an attachment chip removes the whole chip (plus the one trailing space
a paste inserts) in one keystroke, mirroring the chip-aware `Backspace`.

## Completion menu

Typing a partial `/command` or an `@path` mention opens the completion
popup above the composer. The menu follows the IDE-autocomplete contract:

| Key | Behaviour while the menu is open |
|-----|----------------------------------|
| *(menu appears)* | The **first candidate is selected by default** — the solid brand band and the details flyout track it with no prior keystroke |
| `↑` / `↓` | Move the highlight |
| `Enter` | Commit the highlighted candidate |
| `Tab` | Commit the highlighted candidate (same as `Enter`) |
| `Esc` | Dismiss the popup without accepting; the composer text is untouched |
| `Tab` *(after `Esc`)* | Re-open the dismissed menu — the toggle's other half — landing selected on the first candidate again |

The selection is kept coherent by an **anchor pass** that runs wherever the
candidate list is re-derived (per keystroke, after each dispatched action,
and at the render gate): a freshly opened menu seeds its highlight onto the
first candidate, a stale index clamps back into range when the list shrinks,
and no rendered menu (a resolved exact-match composer, a dismissed popup, an
open modal) clears the highlight. The highlighted row can therefore never
point at a menu that is not on screen.

Tab's re-open gesture requires **trigger text** to still be present: a
partial `/command` or a live `@mention` qualifies; a fully-typed known
command (the resolved state whose popup is deliberately hidden) and plain
prose do not, so Tab never resurrects a menu the text no longer asks for.
A keystroke (`InsertChar` / `Backspace`) also re-arms live completions on
its own, clearing the dismissal latch.

## Selection

Semantic mouse-drag selection works on input text via `INPUT_MSG_IDX`
(`usize::MAX - 2`) in the layout map. Copy extracts from `app.input` using
byte-precise ranges. Layout recording is skipped when the API-key modal masks
the display.

## Attachment chips

Pasting an image or a large text block stages the payload behind an inline
**chip** instead of flooding the box (see `tui/composer_attachments.rs`). The
chip label is a real identifier: `#N` keys into the staged payload, the line
count and size badge report what is hidden, and the color marks the kind:

| Chip | Label | Color |
|------|-------|-------|
| Large pasted text | `[Pasted text #1 +42 lines · 12.5 KB]` | Bold `info` blue on a tinted band |
| Pasted image | `[Image #1 · 24.1 KB]` | Bold `warning` amber on a tinted band |

The pill recolors across wrapped rows, keeps its identity color while
selected (the selection still wins the background), and a single `Backspace`
erases the whole chip (plus one trailing space) in one keystroke. The size
badge is re-derived from the staged payload on every reconcile, so a relabeled
chip never reports a stale byte count.

Coloring follows the **real staged state**, not the label text: a chip whose
`#N` has no backing payload — typed by hand (`[Image #1]`), or left over after
the paste was undone — renders as ordinary text, so a literal label never reads
as an attachment that isn't there. The submit path agrees: unbacked chips are
dropped before the model sees them, so what looks plain in the box also never
ships as a fake attachment.

## Visibility

Hidden when overlay modals are open (see [modals](modals.md)). The composer
input is also borrowed by the [model editor](modals.md#model-editor) and
the [history search](modals.md#history-search-modal) modals — they route
keystrokes through the same surface but render their own framing around it.

## History pointer model

The `↑`/`↓` inline recall treats the composer as a **pointer** over three
kinds of slot, so the arrow keys are predictable and nothing you type is ever
silently lost:

| Slot | Contents | Behaviour |
|------|----------|-----------|
| **Draft** (the newest position) | The input that has **not been successfully sent**: what you are composing right now, an input restored by interrupting a round before output (`UnsentInput`), or an entry inserted from Ctrl+R | Editable and **remembered**. Walk into history with `↑` and back with `↓` and the draft comes back exactly as you left it (text + attachments) |
| **Queue row** (an outbox item) | A staged next-round message: a busy-Enter item, or a `Ctrl+O` insert whose round ended before admission | **Editable projection**. `↑`/`↓` walk the pointer across the queue without removing anything; `Enter` writes the edit back **into that item, in place** — the queue's length and order are untouched |
| **History row** `p` | A previously sent prompt from this session's history, newest-first | **Read-only snapshot**. You can edit it before sending, but the edit is temporary — once the pointer moves away, coming back to the row reloads the original text |

Navigation (the queue comes first — it is the newer, more urgent surface):

- With the queue non-empty, `↑` arms the **queue pointer** at the newest item
  and steps toward older items (clamping at the oldest). The first press
  stashes the draft, so a stray `↑` never loses what you were typing.
- `↓` walks the queue pointer back toward newer items; past the newest it
  dissolves the pointer and restores the stashed draft.
- Only an exhausted queue hands `↑` on to input history, where the same
  gestures walk the history rows instead.

**Committing a queue edit.** `Enter` while the pointer is armed writes the
composer's content back into the pointed-at item *in that item's slot*:
editing `a` of a `[a, b, c]` queue into `d` yields `[d, b, c]` — never
`[b, c, d]` (a requeue) and never a duplicate.

**When the pointed-at item vanishes.** The item may ship, be deleted, or be
recalled while you are editing (its round completed behind your back). The
pointer is then empty: your edit stays in the composer, and the next `Enter`
treats it as a **fresh message** — sent immediately if the session is idle,
queued at the back if it is busy. The gesture never dead-ends on a race.

What counts as "the newest / unsent" slot is defined by **send success**:

- Sending a message historicises it — the draft slot is cleared, and the
  message becomes the newest history row (recallable with `↑`).
- Interrupting a round before any output is produced unsends the message: the
  harness reverts the conversation and hands the prompt back, where it is
  **adopted as the draft again** (the newest unsent slot), so `↓` past the
  newest row restores it rather than a stale earlier draft. The unsend
  restore is asynchronous, so it only adopts an **idle** composer — a draft
  you are actively typing wins, and the interrupted prompt stays recoverable
  from the input history (`Ctrl+R` / `↑`).
- Inserting from Ctrl+R likewise **replaces** the draft (an explicit user
  gesture): the adopted input is what `↓` restores, never an older remembered
  draft.

The draft is a per-session, in-memory slot — it survives an accidental `↑`/`↓`
round-trip, but not a restart or a session switch. History rows themselves are
never edited by recall; only the recorded prompt text is shown.

### Session scoping and resume

The rows `↑`/`↓` walk are **bound to the session, not the client window**:

- Rows come from the union of the persisted cross-session history (`history.json`,
  filtered to entries tagged with the current session) and a **transcript-derived
  backfill** — the genuine chat prompts of the conversation on screen, rebuilt
  from the session file. The backfill is derived state and never persisted: the
  session file already is the durable record.
- **Resuming restores history.** After `mutx attach <id>`, `/sessions <id>`, or
  picking a session from `/sessions`, the transcript's own
  prompts are backfilled automatically — including turns typed in another
  client or before this client's `history.json` existed. `↑` in a resumed
  session recalls that session's prompts immediately.
- **Switching never carries state across.** `/new`, `/sessions <id>`,
  and `/fork` reset the pointer, the stashed draft, and the staged attachments:
  the new conversation's composer starts from a clean slate, and what you were
  typing in the previous conversation never leaks into it. Slash commands are
  never part of the recall rows (they are UI gestures, not prompts).
- `Ctrl+R` stays **global**: it searches every prompt across every session and
  workspace, independent of which conversation is on screen.

## Source

`draw_composer` in `render/composer.rs`. Rendered manually (not via a `Block`
widget) so the panel can paint full panel-bg padding rows directly.
`INPUT_MSG_IDX = usize::MAX - 2` is the layout-map
message index reserved for live input selection.
