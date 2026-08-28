# User message

Sent user prompts displayed in the transcript.

## Appearance

```text
  ▌ round 1 · 14:32                       ← header (outside the panel)
  ┃                                       ← top padding (full panel-bg row)
  ┃ typed message text here               ← text row
  ┃                                       ← bottom padding (full panel-bg row)
```

| Attribute | Value |
|-----------|-------|
| Background | `user_panel_bg` (17, 22, 19) — dimmer than input |
| Left/right margin | 2 cols of `app_bg` |
| Accent bar | `┃` in `accent` at column 2 |
| Text color | `text_muted` — signals "read-only, already sent" |
| Text indent | 4 cols (2 margin + `┃` + 1 space) |
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
