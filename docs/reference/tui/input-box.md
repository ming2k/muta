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

The `↑`/`↓` inline history recall treats the composer as a **pointer** over two
kinds of slot, so the arrow keys are predictable and nothing you type is ever
silently lost:

| Slot | Contents | Behaviour |
|------|----------|-----------|
| **Draft** (the newest position) | The input that has **not been successfully sent**: what you are composing right now, an input restored by interrupting a round before output (`UnsentInput`), an entry inserted from Ctrl+R, or a message recalled from the queue | Editable and **remembered**. Walk into history with `↑` and back with `↓` and the draft comes back exactly as you left it (text + attachments) |
| **History row** `p` | A previously sent prompt from this session's history, newest-first | **Read-only snapshot**. You can edit it before sending, but the edit is temporary — once the pointer moves away, coming back to the row reloads the original text |

Navigation:

- `↑` moves the pointer toward **older** rows (clamping at the oldest). The
  first `↑` stashes the draft, so a stray `↑` never loses what you were
  typing.
- `↓` moves the pointer back toward the **newest** row, and pressing it once
  more past the newest row returns to the draft (restoring the stashed text
  and attachments).

What counts as "the newest / unsent" slot is defined by **send success**:

- Sending a message historicises it — the draft slot is cleared, and the
  message becomes the newest history row (recallable with `↑`).
- Interrupting a round before any output is produced unsends the message: the
  harness reverts the conversation and hands the prompt back, where it is
  **adopted as the draft again** (the newest unsent slot), so `↓` past the
  newest row restores it rather than a stale earlier draft.
- Inserting from Ctrl+R or recalling from the queue likewise **replaces** the
  draft: the adopted input is what `↓` restores, never an older remembered
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
- **Resuming restores history.** After `neenee resume`, `/sessions <id>`, or
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
