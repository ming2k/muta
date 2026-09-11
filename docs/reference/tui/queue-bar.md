# Queue bar

One-row persistent summary of the viewed session's staged outbox, pinned in the
footer stack above the transient [activity bar](activity-bar.md) and below the
durability banner. It is the permanent home for outbox state while the agent is
mid-round: a busy `Enter` in follow-up mode stages the message here rather than
sending it immediately.

## Appearance

```text
 FOLLOW-UPS 1  fix the flaky test in the parser        Ctrl-q expand
```

The single row carries, left → right: the identity (a brand-colored
**`FOLLOW-UPS`** label + the **item count**, plus a `blocked` state tag while
the outbox is held), a one-line **preview of the next item to pop** (front of
the FIFO outbox), and the right-pinned **keycap legend**. The legend is
separated from the content by a guaranteed `BAR_LEGEND_GAP_MIN` columns.

| Segment | Content | Style |
|---------|---------|-------|
| Tag | `FOLLOW-UPS` | `theme.brand()` + BOLD |
| Count | item count (`99+` past 99) | `theme.fg()` + BOLD; `theme.warn()` while paused; `theme.err()` while blocked |
| Blocked tag | `blocked` (only when the outbox is blocked), two spaces after the count | `theme.err()` + BOLD |
| Next-item preview | one-line, control-chars-collapsed; truncated with `…`; dropped below 8 columns of budget | `theme.fg()` |
| Legend | the keycap unit for the expand chord (canonically `Ctrl-q`) + ` expand`; omitted entirely while the command has no resolved chord | keycap (`theme.brand()` + BOLD) + `theme.muted()` |

The preview joins the identity with the [join ladder](visual-language.md)'s
enumerated gap and truncates with `…`. The bar sits on the **plain surface**
(no raised tint, no tray glyph, no send-time label); the per-item send time
lives in the [Queue modal](modals.md) instead.

Under width pressure the row degrades in a fixed order: the legend's label
drops (keeping the bare `Ctrl-q` keycap), then the preview shrinks and
disappears, then the whole legend goes — so the identity on the left always
survives.

### Paused vs blocked

There are two distinct "held back" states, surfaced with different colors so
they never read the same:

- **Paused** (count → `theme.warn()`): a staged message is waiting because the
  running round has not yet reached its natural completion. The moment the
  round completes and the harness goes idle, the front item auto-dispatches
  into a fresh round.
- **Blocked** (count → `theme.err()` + a `blocked` tag): the outbox is held
  open. This happens while the Queue panel is open (an editing-safety latch: it
  is set on open and released on close), and it can be toggled by hand from
  inside that panel with `Ctrl+P` — which is the only place that chord reaches
  the outbox, because at the top level `Ctrl+P` is the command palette. While
  blocked, **no** queued message auto-drains, not even after the round completes
  and the harness goes idle: the explicit "send nothing" override.

## Visibility

| Condition | Visible? |
|-----------|----------|
| The viewed session's outbox is non-empty | Yes |
| Empty outbox | No (the row returns to the transcript) |
| Subagent zoom view | No |
| Overlay modal open (Sessions, Models, Help, …) | Yes — overlays float over the conversation (recessing or dimming it); only a full-screen scene or the subagent page removes the whole footer |
| Full-screen scene (Dashboard, Settings) | No |

## Interaction

The bar is a **click target** and its legend is a real binding: clicking it, or
pressing the keycap it shows (canonically `Ctrl+Q`), opens the
[Queue panel](modals.md) — the same panel as `/queue`, or the palette's
"Queue (Outbox)" entry. The legend renders whatever the command registry
resolves for that command, and renders nothing when the command has no chord
(ADR-0238), so the bar can never advertise an affordance that does not fire.

Inside the panel the outbox can be managed without closing it:

| Key | Effect |
|-----|--------|
| `↑` / `↓` | Move the item selection cursor (wrapping), with the body following it |
| `Enter` | Recall the selected item into the composer for re-editing |
| `D` | Delete the selected item |
| `K` / `J` | Move the selected item one slot toward the front / the back |
| `Ctrl+P` | Toggle block/resume for the outbox |
| `Esc` | Close the panel (which releases the latch it set on open) |

Staging itself belongs to the composer: while a round is running, the composer
toggles between *steer* and *follow-up* mode (the `toggle_send_mode` chord,
canonical `Tab`), and `Enter` in follow-up mode queues the message into this bar
instead of sending it. The composer's `↑`/`↓` walk **history**, never this bar
(ADR-0174's arrow-edge hand-off); an outbox item is recalled explicitly, from
the panel. See [input box](input-box.md) and
[composer](../../explanation/composer.md) for that model.

## Source

`draw_queue_bar` in `chrome/queue_bar.rs`. Identity, count, paused/blocked
coloring, the width-pressure legend, and the next-item preview live there; the
staged items are `App::pending_dispatch` entries mirrored into the view via
`QueueItemProps`. The legend's chord arrives on `QueueBarProps::expand_key`,
resolved by the shell from the command registry
(`GlobalOverrides::effective_binding(CommandId::OpenQueue)`) — never a literal
(ADR-0238). The block state lives in `App::queue_blocked_sessions` (set on
panel open, released on close, toggled by the panel's `Ctrl+P`); the auto-drain
gate in the event loop honors it. Height token: `QUEUE_BAR_ROWS = 1`
(`design.rs`); the expand chord is `CommandId::OpenQueue`'s canonical binding
(`Ctrl+Q`, `keymap.rs`).
