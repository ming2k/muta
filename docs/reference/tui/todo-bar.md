# Todo bar

One-row persistent summary of the agent's live task list, shown at the top of
the footer stack — above the [queue bar](queue-bar.md) and the transient
[activity bar](status-bar.md). It is the permanent home for task-list
affordances: the activity bar no longer embeds a todos badge, so the list is
always glanceable while it is non-empty — even while the harness is idle.

## Appearance

```text
 📌 TODOS 2/5 · write the documentation      Ctrl+T expand
```

The bar surfaces three things at a glance: a `📌`-led **`TODOS`** label, the
**done/total progress**, and a one-line **preview of the current item** — the
`InProgress` item, or the first `Pending` item when nothing is mid-flight (it
then reads as "next up"). A right-pinned **`Ctrl+T expand`** legend is the
keyboard affordance for the full list.

| Segment | Content | Style |
|---------|---------|-------|
| Tag | `todo` | `theme.fg()` + BOLD |
| Separator | ` · ` | `theme.muted()` |
| Progress | `done/total` | `theme.fg()` + BOLD |
| Current item preview | `InProgress` item content, else first `Pending`; truncated with `…` | `theme.fg()` |
| Legend | `Ctrl+T` + ` expand` | keycap (`theme.brand()` + BOLD) + `theme.muted()` |

Under width pressure the legend drops the `expand` label (keeping just the
`Ctrl+T` keycap), then drops entirely, so the tag, progress, and preview on
the left always survive. The preview truncates to the remaining width.

## Visibility

| Condition | Visible? |
|-----------|----------|
| Non-empty task list, harness idle | Yes — this is the bar's reason for existing |
| Active round (`responding`, tool work, …) | Yes, when the list is non-empty |
| Empty task list | No (the row returns to the transcript) |
| Envoy zoom view | No |
| Overlay modal open | No |

The bar appears the moment the agent writes its first todo and stays up until
the list empties, so an active plan is visible across the whole round
lifecycle and between rounds.

## Interaction

The whole bar is a click target. Clicking it, or pressing `Ctrl+T`, opens the
[Activity modal](modals.md) pinned to the **Todos** tab, which renders the
full per-item breakdown (one row per item with a status glyph and wrapped
content). The list is agent-owned and read-only in the TUI.

## Source

`draw_todo_bar` in `chrome.rs`. Current-item selection, progress, and the
width-pressure legend live there; the underlying list is
`neenee_core::TodoList` (`crates/neenee-core/src/todos.rs`), mirrored into the
view via `TranscriptView::todos`. Height token: `TODO_BAR_ROWS = 1`
(`design.rs`).
