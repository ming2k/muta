# Queue bar

One-row persistent summary of the viewed session's staged outbox, pinned at the
top of the footer stack just below the [todo bar](todo-bar.md) and above the
transient [activity bar](activity-bar.md). It is the permanent home for
queue-affordances while the agent is mid-round: a busy `Enter` stages the
message here rather than sending it immediately.

## Appearance

```text
 QUEUE 1  fix the flaky test in the parser   Ctrl+O insert  Ctrl+P block  Ctrl+Q expand
```

The single row carries, left → right: the identity (a brand-colored **`QUEUE`**
label + the **item count**, plus a `· blocked` state tag while the user holds
the outbox), a one-line **preview of the next item to pop** (front of the FIFO
outbox), and the right-pinned **keycap legend** (`Ctrl+O` insert into the
running round, `Ctrl+P` block/resume, `Ctrl+Q` expand). The keycap units are
same-rank peers, so they join with plain whitespace (R2 in the [join
ladder](visual-language.md)) — no `·`. The preview truncates with `…`.

| Segment | Content | Style |
|---------|---------|-------|
| Tag | `QUEUE` | `theme.brand()` + BOLD |
| Count | item count (`99+` past 99) | `theme.fg()` + BOLD; `theme.warn()` while paused; `theme.err()` while blocked |
| Blocked tag | `blocked` (only when the outbox is blocked) | `theme.err()` + BOLD |
| Next-item preview | one-line, control-chars-collapsed; truncated with `…` | `theme.fg()` |
| Legend | `Ctrl+O` + ` insert`  `Ctrl+P` + ` block`/` resume`  `Ctrl+Q` + ` expand` | keycap (`theme.brand()` + BOLD) + `theme.muted()` |

The bar sits on the **plain surface** (no raised tint, no tray glyph, no
send-time label), quietly matching the [todo bar](todo-bar.md) above it. The
per-item send time lives in the [Queue modal](modals.md) instead.

The legend keeps a guaranteed `BAR_LEGEND_GAP_MIN` columns of breathing room
from the content. Under width pressure the row degrades in a fixed order: the
preview truncates (down to a minimum, then disappears), the legend labels drop
(keeping the bare keycaps `Ctrl+O  Ctrl+P  Ctrl+Q`), then the `Ctrl+Q`/`Ctrl+O`
clusters drop (keeping just `Ctrl+P`, the state toggle), then the whole legend —
so the identity on the left always survives.

### Paused vs blocked

There are two distinct "held back" states, surfaced with different colors so
they never read the same:

- **Paused** (count → `theme.warn()`): a staged message is waiting because the
  running round has not yet reached its natural completion. The moment the
  round completes and the harness goes idle, the front item auto-dispatches
  into a fresh round.
- **Blocked** (count → `theme.err()` + a `blocked` tag): the user has
  hard-blocked the outbox with `Ctrl+P` (or by having the Queue modal open).
  While blocked, **no** queued message auto-drains — not even after the round
  completes and the harness goes idle. This is the explicit "send nothing"
  override. Press `Ctrl+P` again (the legend flips to `Ctrl+P resume`) to
  release it.

### Mid-round inserts (`Ctrl+O`)

`Ctrl+O` inserts the composed message into the **running** round instead of
staging it for the next one. Unlike every other queued message, an insert is a
**transcript entry from the moment it is sent** (ADR-0126): it lands in the
scrollback as a user panel in the pending treatment (`⏸ Queued` header, dimmer
band) — visibly blocked on the running turn — while the turn's own streaming
entry keeps appending below it. The queue bar is not involved.

The entry settles in place (same id, no duplicate row):

- **Admitted** — the agent crosses a safe turn boundary; the entry flips to
  delivered and renders the `↳ insert` provenance.
- **Round ended first** — the turn completed (naturally, or via an `Esc Esc`
  interrupt; both terminate the round and both are the insert's cue). The
  entry flips to `⏸ Held for next round`, and its content joins the outbox as
  a paused next-round item — visible here, editable with `↑`, and shipped as
  the next round's prompt.

An insert accepts **no queue operations** — no recall, edit, delete, reorder,
or cancel. It has already entered the conversation; only its *held* descendant
(after a round ends first) is a queue item.

## Visibility

| Condition | Visible? |
|-----------|----------|
| The viewed session's outbox is non-empty | Yes |
| Empty outbox | No (the row returns to the transcript) |
| Envoy zoom view | No |
| Overlay modal open | No |

## Interaction

The whole bar is a click target. Clicking it, or pressing `Ctrl+Q`, opens the
[Queue modal](modals.md) — which **auto-blocks** the outbox so the list can be
managed safely (delete, reorder, re-edit) without an item auto-draining
mid-edit. Closing the modal (Esc / outside-click) resumes normal auto-drain.

| Key | Effect |
|-----|--------|
| `Enter` (while busy) | Stage the composed message into the outbox |
| `Ctrl+O` | Insert the composed message into the running round (mid-round insert) |
| `↑` (queue non-empty) | Walk the **queue pointer** to the previous (older) item — the composer becomes an editable projection; nothing leaves the queue |
| `↓` (pointer armed) | Walk the pointer toward newer items; past the newest, restore the draft |
| `Enter` (pointer armed) | Commit the composer's edit back into the pointed-at item **in place** |
| `Ctrl+Q` | Open the Queue modal (auto-blocks the outbox) |
| `Ctrl+P` | Toggle block/resume: hold the whole outbox back, or release it |

`Ctrl+P` is the persistent block toggle — it survives across modal open/close.
The modal's own block is an editing-safety latch: it is set on open and
released on close.

See [input box → queue pointer](input-box.md) for the full pointer model
(what happens when the pointed-at item ships mid-edit).

## Source

`draw_queue_bar` in `chrome.rs`. Identity, count, paused/blocked coloring, the
width-pressure legend, and the next-item preview live there; the staged items
are `neenee_tui::app::QueuedDispatch`, mirrored into the view via
`QueueItemView`. The block state lives in `App::queue_blocked_sessions`; the
auto-drain gate in the event loop honors it. Height token: `QUEUE_BAR_ROWS = 1`
(`design.rs`).
