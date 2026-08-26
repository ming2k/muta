# Activity bar

Transient activity indicator shown in the footer stack, directly above the
input box (below the ambient [todo bar](todo-bar.md) and queue bar). It
unifies the live status label and the breathing-dot liveness anchor into one
click-to-open bar. Long-lived session-state flags (`autopilot` and friends)
are deliberately absent — they live on the dedicated [head row](status-bar.md)
below the hint bar — and the task-list summary lives on its own todo bar above.

## Appearance

```text
 ● making edits (23s · Esc Esc interrupt)
```

The bar surfaces what the user most wants to know mid-round: the **live
status** (lead, shimmering muted → brand sweep) and the **elapsed** timer
inside the interrupt hint. Segments are omitted when there is nothing to
report, so a plain round reads simply:

```text
 ● making edits (3s · Esc Esc interrupt)
```

The structural counters — `round N · turn M · <model>` — no longer live on
the bar. They take space and change rarely, so they moved into the
**Activity modal** that this bar opens on click. The whole bar is a click
target (and `Tab`/`Enter` opens the modal): one glance answers "what's
happening, how long?", one click shows the full breakdown (Activity tab:
current prompt, round/turn/model/elapsed; Todos tab: the task list).

| Attribute | Value |
|-----------|-------|
| Location | 1 row directly above the input box |
| Height | `ACTIVITY_BAR_ROWS = 1` while a round is active, 0 when idle |
| Glyph | `●` (`spinner_glyph`), BOLD |
| Glyph color | `breathing_color(phase, theme.brand(), theme.surface())` — a cosine luminance sweep between brand and surface so the dot breathes at roughly 10 fps instead of cycling braille frames |
| Status text color | `theme.brand()` + ITALIC |
| Elapsed | `theme.muted()` |
| Indent | 1 space |

The breathing sweep is the TUI's single liveness anchor — every other
running indicator (tool step, thinking marker) holds a steady
accent so this dot is the only thing in the user's peripheral vision that
moves. See [ADR-0008](../../adr/0008-single-breathing-anchor.md).

## Visibility

| Condition | Visible? |
|-----------|----------|
| Idle | No — the row returns to the transcript (the task list lives on the [todo bar](todo-bar.md)) |
| Streaming assistant text ("responding") | Yes — the bar stays up across the whole round lifecycle, sustaining the breathing-dot liveness anchor (ADR-0008) through the longest phase |
| Running tool / queued / waiting | Yes |
| Slash command dispatched (harness idle) | No — a command is a synchronous control-plane operation outside the round state machine, so it never arms the bar; its in-flight state is the pending command row in the transcript ([ADR-0110](../../adr/0110-commands-do-not-trigger-the-activity-bar.md)) |
| Overlay modal open | No |

The bar persists from round start (user submits) through every phase —
`queued`, `responding`, tool work, `finalizing response` — and only
disappears when the harness returns to idle. This keeps the breathing dot
in peripheral vision for the entire active round and avoids a layout shift
at the streaming boundary.

## Round and turn

The bar no longer shows the round/turn counters; they live in the Activity
modal (click the bar) as a detail line `round N · turn M · <model> ·
<elapsed>`. See [Rounds and turns](../../explanation/agent-design/rounds-and-turns.md)
for the full concept; in short:

| Counter | Meaning |
|---------|---------|
| `round N` | The user-perceived round number (1-indexed). Bumped once per submitted message. |
| `turn M` | The model-request index within the current round (1-indexed). A turn spans one model request plus the tool work that follows. |

The turn number resets each round; the round number resets only on a new
session.

## Activity labels

| Tool / phase | Label |
|--------------|-------|
| Queued (a chat round admitted, not yet running) | `queued` |
| Waiting for provider | `waiting for model` |
| `read_text` / `find_files` / `list_dir` / `use_skill` | `exploring` |
| `search_text` | `searching codebase` |
| `write_file` / `edit_file` | `making edits` |
| `bash` | `running command` |
| MCP tools (`mcp__*`) | `using MCP` |
| Finalizing stream | `finalizing response` |
| Provider retry | `retry 1/15 · next in 6.6s` (or `running · 1.2s` once active) — error details displayed in Activity modal |

## Source

`draw_activity_bar` in `render/chrome.rs`. Glyph from `spinner_glyph`;
luminance sweep from `breathing_color` in the same module. Spinner phase
driven by `app.spinner_tick` incremented once per frame. Round and turn values
are mirrored from the round-admission and turn-start events by the response
listener.
