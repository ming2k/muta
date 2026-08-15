# Frame layout

How the terminal rect is divided across the TUI's three viewing modes: the
**root conversation**, the **envoy zoom view**, and the **modal overlay**
state. Component-by-component detail lives on each component's own page;
this one owns the rect math, the chrome-hiding rules, and the measurements
table.

## Viewport

Every frame is first filled with `theme.surface()` (`app_bg`) so the TUI
owns every cell rather than leaving gaps at the terminal emulator's default
color. Components then render inside the **viewport**: `frame.area()`
inset by `VIEWPORT_TOP_MARGIN = 1` row at the top only
(`VIEWPORT_BOTTOM_MARGIN = 0`, `VIEWPORT_H_MARGIN = 0`), so components span
the full terminal width and the hint bar pins flush against the terminal's
bottom edge. The top margin row is the only cell row kept as pure `app_bg`
on every frame.

```text
┌──────────────────────────────────────────────────────────────┐
│app_bg  (top viewport margin, 1 row — outside every chunk)    │
├──────────────────────────────────────────────────────────────┤
│                                                              │
│          viewport (everything below, flush to the            │
│               terminal's bottom edge)                        │
│                                                              │
└──────────────────────────────────────────────────────────────┘
```

The viewport rect itself comes from `viewport_rect(frame)` in
`crates/neenee-tui/src/primitives.rs`.

## Root conversation view

The default. A two-chunk vertical split inside `draw_transcript`:

| Chunk | Constraint | Contents |
|-------|-----------|----------|
| Transcript | `Min(0)` | All message content; sticky-pinned step summaries overlay its top row |
| Footer | `Length(footer_height)` | A vertical stack (see below) |

```text
┌──────────────────────────────────────────────────────────────┐
│app_bg  (top viewport margin, 1 row)                          │
├──────────────────────────────────────────────────────────────┤
│                                                              │
│  Transcript viewport                              chunks[0]  │
│   (messages, expandable steps, sticky pinned summaries)      │
│                                                              │
├──────────────────────────────────────────────────────────────┤
│  Activity bar (optional, 0 or 1 row)              ┐          │
│  Todo bar (optional, 0 or 1 row)                   │          │
│  Queue bar (optional, 0 or 2 rows)                 │ chunks[1]│
│  Input box (grows with text, capped)               │          │
│  Hint bar (1 row, persistent)                      │          │
│  Status bar (1 row, persistent)                   ┘          │
└──────────────────────────────────────────────────────────────┘
  (bottom edge: the hint bar pins flush — no bottom viewport margin)
```

The top of every view carries a **head row** — a single-row identity strip.
On the Main session view it shows `SESSION`, the session-id tail, and the
workspace path on the left, plus the session mode (`autopilot`) on the right.
Contextual pages (`/btw`, Envoy, Dashboard) replace it with their own
contextual header. See [Head row](status-bar.md).

### Footer stack

The footer's height is the sum of its rows. The activity, todo, and queue
bars are optional and collapse to 0 when they have nothing to show; the input
box and hint bar are persistent (when chrome is visible):

| Row | Height | When present |
|-----|--------|--------------|
| Activity bar | `ACTIVITY_BAR_ROWS = 1` | Activity is non-empty and not `idle`; not in envoy view; chrome visible. Breathing-dot liveness anchor plus the live status label and the round elapsed timer. Click to open the Activity modal. See [Activity bar](activity-bar.md). |
| Todo bar | `TODO_BAR_ROWS = 1` | A non-empty task list exists; not in envoy view; chrome visible. `TODOS` tag · done/total progress · current-item preview. Click to open the Activity modal on the Todos tab. See [Todo bar](todo-bar.md). |
| Queue bar | `QUEUE_BAR_ROWS = 1` | The viewed session's outbox is non-empty; not in envoy view; chrome visible. `QUEUE` identity · count · inline preview of the next item to pop · key legend (`F4` insert into the running round, `F3` block/resume, `F2` expand). Count turns warning-colored while paused (round not done) and error-colored + `blocked` tag when the user holds the outbox with `F3`. Click to expand the Queue modal (auto-blocks the outbox for safe editing). |
| Input box | `COMPOSER_VERTICAL_CHROME_ROWS + wrapped_lines`, capped at `terminal_height / 2`, min `COMPOSER_MIN_HEIGHT = 3` | Not in envoy view; chrome visible |
| Hint bar | `HINT_BAR_ROWS = 1` | Chrome visible (always, when no modal is open). Carries the next-Enter action (left) and the model/`@instance`/reasoning/context cluster (right). |

```text
┌─────────────────────────────────────────────────────────────┐
│ SESSION b3c4 ~/projects/xx                       autopilot │  ← head row
│ TODOS 2/5 · write the documentation           Ctrl+T expand │  ← todo bar
│ QUEUE 1  {next item preview…}  F4 insert  F3 block  F2 expand │  ← queue bar
│ ● making edits (23s · Esc Esc interrupt)                 │  ← activity bar
│  > type here…                                               │  ← input box
│ Enter send         Kimi K3 max @kimi-code  89.2k (8%)       │  ← hint bar
└─────────────────────────────────────────────────────────────┘
```

The activity bar carries the breathing-dot liveness anchor plus the live
status label and the round elapsed timer — each surfaced only while it
applies. It sits directly above the input box so the live status reads as
part of the composer cluster. The ambient meta bars float above it: the todo
bar leads the stack and owns the agent's live task list (tag · progress ·
current item); the queue bar owns the pending outbox. (Every join on these
rows — the ` · ` between progress and preview, the whitespace between keycap
units — follows the [join ladder](visual-language.md).) The structural counters
(`round N › turn M · <model>`) deliberately do **not** appear on the bars;
they live inside the Activity modal (opened by clicking the activity bar),
along with the per-item todo breakdown. The hint bar carries the next input
action plus model/context info. Session-level state (workspace, mode flags)
lives on the head row at the top of the view, so none of the footer bars
have to carry it. The footer is inset by
`FOOTER_H_INSET = TRANSCRIPT_H_INSET = 2` cols on each side; all rows share
the same horizontal extent so their left and right edges line up.

### Sticky pinned step summary

When an expanded step's body covers the top of the viewport (its summary
has scrolled out of view), the renderer overlays the step's one-line
summary on the top row of the transcript area with a `-` marker. This lets
the user always see which step's body they are looking at, and click to
collapse it, without forcing a scroll anchor. Rendered by
`draw_sticky_summary_if_needed`; see [expandable step](expandable-step.md).

## Envoy zoom view

When the user zooms into an `envoy` tool step, the footer is hidden entirely
and the transcript chunk is split to make room for a one-row navigation bar
at the bottom. The message stream is the focused envoy's child messages,
not the root conversation.

```text
┌──────────────────────────────────────────────────────────────┐
│app_bg  (top viewport margin, 1 row)                          │
├──────────────────────────────────────────────────────────────┤
│                                                              │
│  Transcript viewport (focused task's child messages)         │
│                                                              │
│   …user / assistant / tool steps / thinking steps…           │
│                                                              │
├──────────────────────────────────────────────────────────────┤
│  Task  explore the codebase  (1 of 3)   Esc back  [ prev  ] next │  ← envoy bar
└──────────────────────────────────────────────────────────────┘
  (bottom edge: the envoy bar pins flush — no bottom viewport margin)
```

| Region | Constraint | Height |
|--------|-----------|--------|
| Transcript (children) | `Min(0)` | fills |
| Envoy bar | `Length(ENVOY_BAR_ROWS = 1)` | 1 |

The activity bar, todo bar, queue bar, input box, and hint bar
all collapse to 0 — the zoomed view is read-only, with the navigation bar as
its only chrome.
See [Envoy view](envoy-view.md) for the focus stack that drives this
mode and the bar's contents.

## Modal overlay view

When an overlay modal is open, its recess policy (`Modal::recess`) decides
what happens to the surface beneath it. A **Dim** modal keeps the footer at
its normal height and darkens the whole live surface in place
(`recess_backdrop` scales every cell by `theme.modal_dim_factor`), so the
transcript and chrome stay visible for context while the centered panel reads
as the focal layer. The **Takeover** policy (the sessions picker only) instead
collapses the entire footer (activity bar, todo bar, queue bar, input box,
hint bar) to 0 height and fully occludes the surface. The one
**None**-recess surface is the [permission sheet](modals.md#permission-sheet),
which is inline (no dimming, no footer collapse) and replaces only the
input-box area.

```text
┌──────────────────────────────────────────────────────────────┐
│                                                              │
│             dimmed (visible) transcript surface           │
│                                                              │
│            ╭────────────────────────────────────╮            │
│            │                                    │            │
│            │       centered overlay modal       │            │
│            │                                    │            │
│            ╰────────────────────────────────────╯            │
│                                                              │
│footer = 0 (activity, state, input, hint bars all hidden)      │
└──────────────────────────────────────────────────────────────┘
```

See [modals](modals.md) for which modal uses which `centered_rect`
percentage and which (rare) overlays keep the chrome visible.

## Horizontal gutters

Every transcript-area component is inset by `TRANSCRIPT_H_INSET = 2` cols
on each side so no band, bar, or text touches the terminal frame. The two
gutters stay `app_bg` via the global frame fill. Solid-background regions
(code blocks, child tool steps) render into `transcript_band_rect`
(`view.rs`), which is the transcript area minus both gutters; user
panels and code blocks render their own equivalent gutters; markdown text
wraps with `TRANSCRIPT_H_INSET` cells of slack on the right.

```text
┌──────────────────────────────────────────────────────────────┐
│columns: 0 1 2 3                                 ... W-1      │
│          v v v v                                 v           │
│                                                              │
│          app_bg |    transcript band              | app_bg   │
│                                                              │
│          ..  .. +-------------------------------+ ..  ..     │
│          ..  .. |  step header / body / text     | ..  ..     │
│          ..  .. +-------------------------------+ ..  ..     │
│                                                              │
│          <- INSET=2 ->|<-- usable width -->|<- INSET=2 ->    │
└──────────────────────────────────────────────────────────────┘
```

The footer shares the same inset (`FOOTER_H_INSET = TRANSCRIPT_H_INSET`),
so the activity bar, input box, and hint bar all line up
with the transcript content above.

## Transcript viewport behavior

- Messages render top-to-bottom with semantic boundary spacing. A turn header,
  thinking segment, tool batch, and assistant text are separated by one row;
  tool-like siblings in the same known turn are flush.
- Auto-follow pins to the newest content while `follow_bottom` is set.
- Scrolling up pauses follow; scrolling back to the bottom (or sending a
  message) re-engages it.
- `PageUp` / `PageDown` step by `view_height - 1` (one line of overlap);
  mouse wheel steps by 4 rows.

## Key measurements

| Measurement | Value | Where |
|------------|-------|-------|
| Top viewport margin | 1 row (`app_bg`) | `VIEWPORT_TOP_MARGIN` |
| Bottom viewport margin | 0 rows — chrome pins flush to the terminal's bottom edge | `VIEWPORT_BOTTOM_MARGIN` |
| Left/right viewport margin | 0 cols | `VIEWPORT_H_MARGIN` |
| Left/right gutter (all content) | 2 cols `app_bg` | `TRANSCRIPT_H_INSET`, applied via `transcript_band_rect` (steps) / explicit spans (user panel, code block) / wrap-width slack (markdown) |
| Footer side inset | 2 cols (matches `TRANSCRIPT_H_INSET`) | `FOOTER_H_INSET` |
| Activity bar height | 1 row | `ACTIVITY_BAR_ROWS` |
| Todo bar height | 1 row | `TODO_BAR_ROWS` |
| Queue bar height | 1 row | `QUEUE_BAR_ROWS` |
| Hint bar height | 1 row | `HINT_BAR_ROWS` |
| Status bar height | 1 row | `STATUS_BAR_ROWS` |
| Envoy bar height | 1 row | `ENVOY_BAR_ROWS` |
| Input box min height | 3 rows (top transition + 1 text + bottom transition) | `COMPOSER_MIN_HEIGHT` |
| Input box max height | `terminal_height / 2` | `COMPOSER_MAX_HEIGHT_DIVISOR` |
| Input box vertical chrome | 2 rows (top + bottom transition) | `COMPOSER_VERTICAL_CHROME_ROWS` |
| Input box left prefix | 2 cols (`>` + space, or wrap-aligned indent) | `COMPOSER_PROMPT_PREFIX_COLS` |
| Input box right pad | 2 cols | `COMPOSER_RIGHT_PAD_COLS` |
| `┃` bar column | 2 (after 2-col gutter) | User messages, code blocks, input |
| Assistant text indent | 4 cols (left) + 2-col right gutter | `TRANSCRIPT_BODY_PREFIX_COLS`; wraps at `area.width - 6` |
| Code block indent | 2 cols (inside band) + `┃` + space | `code_gutter_line(left_indent=2)` |
| Step marker column | 2 (inside `TRANSCRIPT_H_INSET` band) | `+` / `-` at col 0 of the inset region |
| Step header text column | 4 (2 gutter + 2 after `+ `) | After `+ ` prefix |
| Step body indent | 4 cols from transcript edge | `draw_tool_step`, `draw_reasoning_trace` |
| Line-number gutter min width | 2 chars | `.max(2)` |
| Turn header → first component | 1 row | `TURN_HEADER_BODY_GAP_ROWS` |
| Thinking header → expanded body | 0 rows | `REASONING_TRACE_BODY_TOP_GAP_ROWS` |
| Same-turn tool batch | 0 rows between tool-like siblings | Semantic boundary rule |
| Other component boundaries | 1 row | `MESSAGE_GAP_ROWS` |
| Mouse scroll step | 4 rows | `ScrollUp`/`Down` handler |
| PageUp/PageDown step | `view_height - 1` | One line of overlap |

## Source

| File | Responsibility |
|------|----------------|
| `view.rs` | `draw_transcript` — viewport fill, two-chunk split, footer stack, envoy split, sticky summary overlay |
| `render/design.rs` | All non-color layout tokens: `VIEWPORT_*`, `TRANSCRIPT_*`, `FOOTER_H_INSET`, `ACTIVITY_BAR_ROWS`, `TODO_BAR_ROWS`, `QUEUE_BAR_ROWS`, `HINT_BAR_ROWS`, `STATUS_BAR_ROWS`, `ENVOY_BAR_ROWS`, `COMPOSER_*`, `MESSAGE_GAP_ROWS` |
| `primitives.rs` | `viewport_rect`, `centered_rect`, `panel_block`, `recess_backdrop` |
| `render/chrome.rs` | `draw_activity_bar` (breathing dot + status + elapsed), `draw_todo_bar` (task-list summary), `draw_queue_bar` (outbox summary), `draw_hint_bar` / `HintBarView`, `draw_completion_menu` |
| `tui/page_header.rs` | `draw_page_header` / `PageHeader` / `SessionHead` — the unified head row at the top of every view |
| `render/composer.rs` | `draw_composer` (input box), `INPUT_MSG_IDX` |
| `disclosure/renderers.rs` | `draw_envoy_bar`, `draw_sticky_summary_if_needed` |
| `app.rs` | `in_envoy_view`, `focus_stack`, `follow_bottom`, scroll clamping |
