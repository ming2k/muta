# User message

Sent user prompts displayed in the transcript.

## Appearance

```text
  < round 1  14:32                        ← header (outside the panel)
                                          ← top padding (full panel-bg row)
    typed message text here               ← text row
                                          ← bottom padding (full panel-bg row)
```

### Prompt Variants

All user prompts share the same visual container and `<` Stdin indicator rail, differentiated cleanly by their header metadata:

| Variant | State | Header Format | Description |
| :--- | :--- | :--- | :--- |
| **Normal** | Delivered | `< round 1  14:32` | Standard prompt initiating a round |
| **Steer** | Delivered | `< steer  round 1 › turn 2  14:35` | In-flight steering input with breadcrumb provenance |
| **Steer** | Queued | `< steer  queued  14:35` | Pending steer awaiting turn admission |
| **Follow-up** | Delivered | `< follow-up  round 2  14:36` | Sequential follow-up prompt |
| **Follow-up** | Queued | `< follow-up  queued  14:36` | Pending follow-up awaiting dispatch |

| Attribute | Value |
|-----------|-------|
| Lead indicator | `<` in `theme.info()` bold (Unix stdin redirection) |
| Header style | Upright `MetaStrip` (no italic, no warning colors for status) |
| Background | `user_panel_bg` (`user_panel_bg_queued` when queued) |
| Left/right margin | `USER_MESSAGE_OUTER_GUTTER_COLS` (2 cols) |
| Text color | `theme.user_text()` (`theme.muted()` when queued) |
| Top/bottom padding | Full panel-bg rows (no half-block glyphs — identical across terminals) |

## Selection

Character-level semantic selection — only the dragged substring gets
`selected_bg`, not the whole line. Copy returns the display text verbatim.

## Contrast with input box

| Property | User message | Input box |
|----------|-------------|-----------|
| Shape | Filled panel (`user_panel_bg`) | Filled panel (`input_bg_active` / `input_bg_inactive` when blurred) |
| Text color | `text_muted` | `text` |
| Editable | No | Yes |

## Source

`draw_message_body` → `Block::Text` with `is_user == true` in
`message_body.rs`.
