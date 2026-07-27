# Queue bar

Two-row persistent summary of the viewed session's staged outbox, pinned at the
top of the footer stack just below the [todo bar](todo-bar.md) and above the
transient [activity bar](activity-bar.md). It is the permanent home for
queue-affordances while the agent is mid-round: a busy `Enter` stages the
message here rather than sending it immediately.

## Appearance

```text
 📤 QUEUE 1 · 14:02           F3 block · F2 expand
 fix the flaky test in the parser
```

Row 1 carries the identity: a `📤`-led **`QUEUE`** label, the **item count**,
the **send time of the next item to pop** (local `HH:MM`), and a right-pinned
**keycap legend** (`F3` block/resume, `F2` expand). Row 2 is a one-line
**preview of the next item to pop** (front of the FIFO outbox), truncated with
`…`.

| Segment | Content | Style |
|---------|---------|-------|
| Tag | `📤` glyph + `QUEUE` | `theme.brand()` + BOLD (the "pin" treatment) |
| Count | item count (`99+` past 99) | `theme.fg()` + BOLD; `theme.warn()` while paused; `theme.err()` while blocked |
| Separator | ` · ` | `theme.muted()` |
| Next-item time | local `HH:MM` of the next item to pop | `theme.muted()` |
| Blocked tag | `blocked` (only when the outbox is blocked) | `theme.err()` + BOLD |
| Legend | `F3` + ` block`/` resume` · `F2` + ` expand` | keycap (`theme.brand()` + BOLD) + `theme.muted()` |
| Next-item preview | one-line, control-chars-collapsed; truncated with `…` | `theme.fg()` |

The whole bar sits on a subtly **raised** surface (`theme.raised()`), mirroring
the todo bar, so it reads as a distinct pinned panel rather than another
footer strip.

Under width pressure the right legend degrades in a fixed order: the `expand`
label drops (keeping `F3 block · F2`), then the `block`/`resume` label (keeping
the bare keycaps `F3 · F2`), then the `F2` cluster (keeping just `F3`), then
the whole legend — so the identity on the left always survives. The preview
truncates to the remaining width.

### Paused vs blocked

There are two distinct "held back" states, surfaced with different colors so
they never read the same:

- **Paused** (count → `theme.warn()`): a staged message is waiting because the
  running round has not yet reached its natural completion. There is **no**
  mid-round insert path (the Tab toggle was removed). The moment the round
  completes and the harness goes idle, the front item auto-dispatches into a
  fresh round.
- **Blocked** (count → `theme.err()` + a `blocked` tag): the user has
  hard-blocked the outbox with `F3` (or by having the Queue modal open). While
  blocked, **no** queued message auto-drains — not even after the round
  completes and the harness goes idle. This is the explicit "send nothing"
  override. Press `F3` again (the legend flips to `F3 resume`) to release it.

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
auto-drain gate in the event loop honors it. Height token: `QUEUE_BAR_ROWS = 2`
(`design.rs`).
