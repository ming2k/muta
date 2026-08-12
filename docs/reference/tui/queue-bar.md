# Queue bar

One-row persistent summary of the viewed session's staged outbox, pinned at the
top of the footer stack just below the [todo bar](todo-bar.md) and above the
transient [activity bar](activity-bar.md). It is the permanent home for
queue-affordances while the agent is mid-round: a busy `Enter` stages the
message here rather than sending it immediately.

## Appearance

```text
 QUEUE 1  fix the flaky test in the parser   F4 insert  F3 block  F2 expand
```

The single row carries, left → right: the identity (a brand-colored **`QUEUE`**
label + the **item count**, plus a `· blocked` state tag while the user holds
the outbox), a one-line **preview of the next item to pop** (front of the FIFO
outbox), and the right-pinned **keycap legend** (`F4` insert into the running
round, `F3` block/resume, `F2` expand). The keycap units are same-rank peers,
so they join with plain whitespace (R2 in the [join
ladder](visual-language.md)) — no `·`. The preview truncates with `…`.

| Segment | Content | Style |
|---------|---------|-------|
| Tag | `QUEUE` | `theme.brand()` + BOLD |
| Count | item count (`99+` past 99) | `theme.fg()` + BOLD; `theme.warn()` while paused; `theme.err()` while blocked |
| Blocked tag | `blocked` (only when the outbox is blocked) | `theme.err()` + BOLD |
| Next-item preview | one-line, control-chars-collapsed; truncated with `…`; an in-flight steer leads with a `steer›` badge | `theme.fg()` |
| Legend | `F4` + ` insert`  `F3` + ` block`/` resume`  `F2` + ` expand` | keycap (`theme.brand()` + BOLD) + `theme.muted()` |

The bar sits on the **plain surface** (no raised tint, no tray glyph, no
send-time label), quietly matching the [todo bar](todo-bar.md) above it. The
per-item send time lives in the [Queue modal](modals.md) instead.

The legend keeps a guaranteed `BAR_LEGEND_GAP_MIN` columns of breathing room
from the content. Under width pressure the row degrades in a fixed order: the
preview truncates (down to a minimum, then disappears), the legend labels drop
(keeping the bare keycaps `F4  F3  F2`), then the `F4`/`F2` clusters drop
(keeping just `F3`, the state toggle), then the whole legend — so the identity
on the left always survives.

### Paused vs blocked

There are two distinct "held back" states, surfaced with different colors so
they never read the same:

- **Paused** (count → `theme.warn()`): a staged message is waiting because the
  running round has not yet reached its natural completion. The moment the
  round completes and the harness goes idle, the front item auto-dispatches
  into a fresh round.
- **Blocked** (count → `theme.err()` + a `blocked` tag): the user has
  hard-blocked the outbox with `F3` (or by having the Queue modal open). While
  blocked, **no** queued message auto-drains — not even after the round
  completes and the harness goes idle. This is the explicit "send nothing"
  override. Press `F3` again (the legend flips to `F3 resume`) to release it.

### Mid-round steers (`steer›`)

`F4` inserts the composed message into the **running** round instead of
staging it for the next one: the message is handed to the agent and admitted
as a visible user steer at its next safe turn boundary. While admission is
pending the item stays in the outbox marked with a **`steer›` badge** so it
never reads as an ordinary next-round entry. If the round ends before the
steer crosses a boundary (a race), the item returns to the queue as a paused
next-round entry — nothing is lost.

## Visibility

| Condition | Visible? |
|-----------|----------|
| The viewed session's outbox is non-empty | Yes |
| Empty outbox | No (the row returns to the transcript) |
| Envoy zoom view | No |
| Overlay modal open | No |

## Interaction

The whole bar is a click target. Clicking it, or pressing `F2`, opens the
[Queue modal](modals.md) — which **auto-blocks** the outbox so the list can be
managed safely (delete, reorder, re-edit) without an item auto-draining
mid-edit. Closing the modal (Esc / outside-click) resumes normal auto-drain.

| Key | Effect |
|-----|--------|
| `Enter` (while busy) | Stage the composed message into the outbox |
| `F4` | Insert the composed message into the running round (mid-round steer) |
| `↑` (in the composer, empty input) | Recall the newest queued message back into the composer for editing |
| `F2` | Open the Queue modal (auto-blocks the outbox) |
| `F3` | Toggle block/resume: hold the whole outbox back, or release it |

`F3` is the persistent block toggle — it survives across modal open/close. The
modal's own block is an editing-safety latch: it is set on open and released on
close.

## Source

`draw_queue_bar` in `chrome.rs`. Identity, count, paused/blocked coloring, the
width-pressure legend, and the next-item preview live there; the staged items
are `crate::tui::app::QueuedDispatch`, mirrored into the view via
`QueueItemView`. The block state lives in `App::queue_blocked_sessions`; the
auto-drain gate in the event loop honors it. Height token: `QUEUE_BAR_ROWS = 1`
(`design.rs`).
