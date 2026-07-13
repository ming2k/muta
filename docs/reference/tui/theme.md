# Color palette

All colors are defined in `Theme::default()` (`crates/neenee-tui-view/src/render/theme.rs`).

## Backgrounds

| Token | RGB | Purpose |
|-------|-----|---------|
| `app_bg` | (7, 8, 8) | Darkest base; fills the entire frame |
| `backdrop` | (3, 4, 4) | Dim overlay behind modals (darker than `app_bg`) |
| `code_bg` | (17, 19, 18) | Code blocks and tool-step results |
| `user_panel_bg` | (17, 22, 19) | Sent user-message band (dimmer than input) |
| `user_panel_bg_queued` | (9, 12, 11) | User message staged in the send queue (dimmer = "pending") |
| `panel_bg` | (14, 15, 15) | Modals / sheets |
| `input_bg` | (18, 19, 19) | Live input box (brighter than `user_panel_bg` = "editable") |
| `menu_bg` | (17, 19, 18) | Suggestion / completion menus |
| `element_bg` | (21, 23, 22) | Footer / option bars |
| `selected_bg` | (38, 48, 44) | Semantic-selection highlight |

## Foregrounds

| Token | RGB | Purpose |
|-------|-----|---------|
| `text` | (213, 213, 205) | Primary text (input box, selected); also assistant prose |
| `text_muted` | (119, 125, 117) | Sent messages, labels, secondary text |
| `text_hover` | (175, 180, 172) | Collapsed step header under pointer (between muted and fg) |
| `user_fg` | (165, 177, 164) | User message text |
| `system_fg` | (111, 116, 110) | System / harness messages |
| `code_fg` | (166, 178, 163) | Code content |
| `heading_fg` | (190, 194, 181) | Markdown headings |
| `quote_fg` | (156, 145, 118) | Blockquotes |
| `dim_fg` | (94, 99, 94) | Line-number gutter, tool name |
| `primary` | (142, 161, 145) | Brand / selection; hint-line keys; `┃` bars; breathing-dot indicator |
| `success` | (117, 148, 117) | Completed tool status; context-usage indicator < 70% |
| `info` | (128, 153, 156) | Running tool status, thinking marker |
| `warning` | (181, 149, 93) | Warnings; context-usage indicator 70–90% |
| `error_fg` | (190, 111, 104) | Failed tool status; context-usage indicator > 90% |

## Modifiers

| Field | Default | Purpose |
|-------|---------|---------|
| `modal_dim_factor` | `0.5` | Brightness multiplier (0.0–1.0) applied to every cell of the live surface while a **Dim**-recess modal is open. The terminal cannot alpha-blend, so a dim-recess modal darkens the transcript/chrome in place by scaling each color by this factor — lower is darker. See [Modals](modals.md). |

## Background hierarchy

```text
backdrop (3,4,4)              ← dimmest; modal overlay
app_bg (7,8,8)                ← base; entire frame
  code_bg (17,19,18)              ← code blocks / tool-step results
  user_panel_bg (17,22,19)        ← sent messages (dimmer = read-only)
  user_panel_bg_queued (9,12,11)  ← queued user messages (dimmer = pending)
  panel_bg (14,15,15)             ← modals / sheets
  input_bg (18,19,19)             ← live input box (brighter = editable)
  menu_bg (17,19,18)              ← menus / suggestion popups
  element_bg (21,23,22)           ← footer / option bars (brightest panel)
selected_bg (38,48,44)        ← selection highlight
```

The header is a floating half-block panel on `input_bg` (same as the input
box), inset from the edges by `app_bg` gutters; no separator rules are drawn.

## Diff banding

Every block-level code/text surface shares one design contract (see the
disclosure module): colors flow through theme tokens rather than inline
`Color::Rgb` literals, so retuning the palette in one place retunes every
block.

| Token | RGB | Purpose |
|-------|-----|---------|
| `diff_add_bg` | (18, 31, 22) | Low-chroma row tint for added blocks |
| `diff_del_bg` | (32, 20, 20) | Low-chroma row tint for removed blocks |
| `diff_add_hl` | (42, 64, 48) | Brighter per-word highlight on added rows |
| `diff_del_hl` | (64, 40, 40) | Brighter per-word highlight on removed rows |
