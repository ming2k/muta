# Color palette & Theme System

All built-in colors are defined in `Theme::default()` (`apps/tui/crates/mutx/src/theme.rs`).

## Design Token Architecture

The theme engine adheres to a **3-tier design token architecture**:

```text
┌──────────────────────────────────────────────────────────┐
│ Tier 1: Foundation Palette (8 core semantic swatches)    │
│ [colors]: background, surface, text, muted, accent, ...  │
└────────────────────────────┬─────────────────────────────┘
                             │
                             ▼
┌──────────────────────────────────────────────────────────┐
│ Tier 2: Semantic Derivations                             │
│ Luminance & chroma mixing (body, raised, panel, etc.)    │
└────────────────────────────┬─────────────────────────────┘
                             │
                             ▼
┌──────────────────────────────────────────────────────────┐
│ Tier 3: Component Token Overrides (optional)             │
│ [components.input], [components.crate], [components.diff]│
└──────────────────────────────────────────────────────────┘
```

---

## 1. Backgrounds & Surfaces

| Token | RGB (Zen Default) | Purpose |
|-------|-------------------|---------|
| `app_bg` | (7, 8, 8) | Darkest base; fills the entire frame |
| `backdrop` | (3, 4, 4) | Dim overlay behind modals (darker than `app_bg`) |
| `code_bg` | (17, 19, 18) | Code blocks and tool-step results |
| `user_panel_bg` | (17, 22, 19) | Sent user-message band (dimmer than input) |
| `user_panel_bg_queued` | (9, 12, 11) | User message staged in the send queue (dimmer = "pending") |
| `panel_bg` | (14, 15, 15) | Modals / sheets |
| `input_bg_active` | (26, 28, 27) | Live input-box surface while the box owns the keyboard |
| `input_bg_inactive` | (16, 17, 17) | Live input-box surface while a transcript step owns the keyboard (recessed = inert) |
| `menu_bg` | (17, 19, 18) | Suggestion / completion menus |
| `element_bg` | (21, 23, 22) | Footer / option bars |
| `selected_bg` | (38, 48, 44) | Semantic-selection highlight |

---

## 2. Foregrounds & Accents

| Token | RGB (Zen Default) | Purpose |
|-------|-------------------|---------|
| `text` | (213, 213, 205) | Primary text (input box, selected); also assistant prose |
| `text_muted` | (119, 125, 117) | Sent messages, labels, secondary text |
| `text_hover` | (175, 180, 172) | Reserved intermediate hover tone (the step scheme uses `affordance_fg`, ADR-0174) |
| `affordance_fg` | (150, 163, 150) | Transient affordance hue: collapsed step summary under the pointer or keyboard focus (ADR-0174) |
| `user_fg` | (165, 177, 164) | User message text |
| `system_fg` | (111, 116, 110) | System / harness messages |
| `code_fg` | (166, 178, 163) | Code content |
| `heading_fg` | (190, 194, 181) | Markdown headings |
| `quote_fg` | (156, 145, 118) | Blockquotes |
| `dim_fg` | (94, 99, 94) | Line-number gutter, tool name |
| `primary` | (142, 161, 145) | Brand / selection; model-bar keys; `┃` bars; breathing-dot indicator |
| `success` | (117, 148, 117) | Completed tool status; context-usage indicator < 70% |
| `info` | (128, 153, 156) | Running tool status, thinking marker |
| `warning` | (181, 149, 93) | Warnings; context-usage indicator 70–90% |
| `error_fg` | (190, 111, 104) | Failed tool status; context-usage indicator > 90% |

---

## 3. Dedicated Component Tokens

Specialized UI components have dedicated tokens and fallback constants:

| Component Token | Accessor | Default Constant | Purpose |
|-----------------|----------|------------------|---------|
| `caret_fg` | `theme.caret()` | `DEFAULT_CARET_FG` (213, 213, 205) | Caret / cursor insertion point color in the input field |
| `input_selection_bg` | `theme.input_selection()` | Derived / (38, 48, 44) | Background highlight for selected text in input fields |
| `input_placeholder_fg` | `theme.input_placeholder()` | `DEFAULT_INPUT_PLACEHOLDER_FG` (119, 125, 117) | Color for empty input placeholder prompts |
| `crate_fg` | `theme.crate_tag()` | `DEFAULT_CRATE_FG` (180, 190, 254) | Foreground identifier for crate / cargo tags |
| `crate_bg` | `theme.crate_badge()` | (25, 27, 34) | Background badge pill behind crate tags |
| `keycap_fg` | `theme.keycap_fg()` | `DEFAULT_KEYCAP_FG` (226, 228, 220) | Foreground for keycap labels (crisp high-luminance neutral) |
| `keycap_bg` | `theme.keycap_bg()` | `DEFAULT_KEYCAP_BG` (28, 31, 29) | Micro-elevated background for keycap badge/pill treatments |
| `keycap_label_fg` | `theme.keycap_label()` | `DEFAULT_KEYCAP_LABEL_FG` (158, 166, 155) | High-contrast readable foreground for affordance action labels |
| `keycap_accent_fg` | `theme.keycap_accent()` | `DEFAULT_KEYCAP_ACCENT_FG` (163, 184, 153) | Primary submit/action keycap accent tone (e.g. Enter send) |
| `keycap_warn_fg` | `theme.keycap_warn()` | `DEFAULT_KEYCAP_WARN_FG` (201, 165, 110) | Interrupt/danger keycap tone (e.g. Esc Esc, Ctrl+C) |

---

## 4. Custom Themes Directory & TOML Schema

`mutx` automatically loads all custom theme files located in its themes directory:

```text
~/.config/mutx/themes/*.toml
```

Each theme file adheres to the clean 4-scope `ThemeFile` schema:

### TOML Schema Structure

```toml
# Metadata
name = "Cyberpunk Obsidian"
description = "High-contrast modern cyberpunk palette"

# 1. Foundation Palette (8 hex colors required)
[palette]
background = "#090a10"
surface    = "#141724"
text       = "#e6edf3"
muted      = "#7d8590"
accent     = "#00f0ff"
success    = "#00ff88"
warning    = "#ffd700"
error      = "#ff0055"

# 2. Spatial Surfaces (Layer 0 to Layer 3 overrides)
[surfaces.view]
canvas    = "#090a10"
header_bg = "#10121d"

[surfaces.sheet]
surface = "#181c2d"
border  = "#2d3552"

[surfaces.modal]
surface    = "#141724"
border     = "#00f0ff"
dim_factor = 0.55

# 3. Feedback Tones (container & border)
[feedback.warning]
container = "#26200a"
border    = "#ffd700"

[feedback.error]
container = "#2b0d18"
border    = "#ff0055"

# 4. Component Token Overrides (optional)
[components.input]
bg_active   = "#1f2438"
bg_inactive = "#121522"
caret       = "#00f0ff"
selection   = "#2d3552"
placeholder = "#7d8590"

[components.crate]
fg       = "#ff00a0"
badge_bg = "#2a152e"

[components.diff]
add_bg = "#0d2b1a"
del_bg = "#2b0d18"
add_hl = "#1b5433"
del_hl = "#541b30"

[components.command]
idle_bg  = "#10121d"
hover_bg = "#181c2d"

[components.keycap]
key_fg    = "#ffffff"
key_bg    = "#181c2d"
label_fg  = "#a0a8b6"
accent_fg = "#00f0ff"
warn_fg   = "#ffd700"
```

### Loading & Discovery

1. Custom theme files placed in `~/.config/mutx/themes/` are dynamically parsed on launch.
2. The stem of the filename (e.g. `cyberpunk.toml` → `cyberpunk`) becomes the unique theme ID.
3. Themes appear automatically in the full-screen Settings View (`/config` › Appearance) with live transcript and component swatches.
4. Themes can be selected directly in the UI or specified in `~/.config/mutx/config.toml`:

```toml
color_scheme = "cyberpunk"
```
