# Queue bar

Two-row persistent summary of the viewed session's staged outbox, pinned at the
top of the footer stack just below the [todo bar](todo-bar.md) and above the
transient [activity bar](activity-bar.md). It is the permanent home for
queue-affordances while the agent is mid-round: a busy `Enter` stages the
message here rather than sending it immediately.

## Appearance

```text
 📤 QUEUE 1 · 14:02           Esc recall · F2 expand
 fix the flaky test in the parser
```

Row 1 carries the identity: a `📤`-led **`QUEUE`** label, the **item count**,
the **send time of the next item to pop** (local `HH:MM`), and a right-pinned
**keycap legend** (`Esc` recall, `F2` expand). Row 2 is a one-line **preview of
the next item to pop** (front of the FIFO outbox), truncated with `…`.

| Segment | Content | Style |
|---------|---------|-------|
| Tag | `📤` glyph + `QUEUE` | `theme.brand()` + BOLD (the "pin" treatment) |
| Count | item count (`99+` past 99) | `theme.fg()` + BOLD; `theme.warn()` while paused |
| Separator | ` · ` | `theme.muted()` |
| Next-item time | local `HH:MM` of the next item to pop | `theme.muted()` |
| Legend | `Esc` + ` recall` · `F2` + ` expand` | keycap (`theme.brand()` + BOLD) + `theme.muted()` |
| Next-item preview | one-line, control-chars-collapsed; truncated with `…` | `theme.fg()` |

The whole bar sits on a subtly **raised** surface (`theme.raised()`), mirroring
the todo bar, so it reads as a distinct pinned panel rather than another
footer strip.

Under width pressure the right legend degrades in a fixed order: the `expand`
label drops (keeping `Esc recall · F2`), then the `recall` label (keeping the
bare keycaps `Esc · F2`), then the `F2` cluster (keeping just `Esc`), then the
whole legend — so the identity on the left always survives. The preview
truncates to the remaining width.

### Paused state

A staged message always waits for the running round to finish naturally before
starting a new one (there is **no** mid-round insert path — the Tab toggle was
removed). While items are held back because the round has not yet reached its
natural completion, the count recolors to `theme.warn()` so the user can see the
queue is paused, not forgotten. The moment the round completes and the harness
goes idle, the front item auto-dispatches into a fresh round.

## Visibility

| Condition | Visible? |
|-----------|----------|
| The viewed session's outbox is non-empty | Yes |
| Empty outbox | No (the row returns to the transcript) |
| Envoy zoom view | No |
| Overlay modal open | No |

## Interaction

The whole bar is a click target. Clicking it, or pressing `F2`, opens the
[Queue modal](modals.md), which lists every staged dispatch for the viewed
session (front pops first) with its queued time and truncated text.

| Key | Effect |
|-----|--------|
| `Enter` (while busy) | Stage the composed message into the outbox |
| `Esc` | Recall the newest staged item back into the composer for editing |
| `↑` (in the composer) | Same recall — pull the newest queued message back to edit |
| `F2` | Open the Queue modal |

Recall always restores the newest staged message locally and immediately: since
every item is a next-round one, there is no agent-side insert to cancel.

## Source

`draw_queue_bar` in `chrome.rs`. Identity, count, paused coloring, the
width-pressure legend, and the next-item preview live there; the staged items
are `crate::tui::app::QueuedDispatch`, mirrored into the view via
`QueueItemView`. Height token: `QUEUE_BAR_ROWS = 2` (`design.rs`).
