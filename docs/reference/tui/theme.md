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
| `input_bg_active` | (26, 28, 27) | Live input box while it owns the keyboard (brightest interactive surface = "typing lands here") |
| `input_bg_inactive` | (16, 17, 17) | Live input box while a transcript step owns the keyboard (recessed = inert) |
| `menu_bg` | (17, 19, 18) | Suggestion / completion menus |
| `element_bg` | (21, 23, 22) | Footer / option bars |
| `selected_bg` | (38, 48, 44) | Semantic-selection highlight |

---

## 2. Foregrounds & Accents

| Token | RGB (Zen Default) | Purpose |
|-------|-------------------|---------|
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

---

## 4. Custom Themes Directory & TOML Schema

`mutx` automatically loads all custom theme files located in its themes directory:

```text
~/.config/mutx/themes/*.toml
```

Each theme file must adhere to the formal `ThemeFile` contract:

### TOML Schema Structure

```toml
# Metadata
name = "Cyberpunk Neon"
description = "High-contrast neon cyberpunk palette"

# 1. Foundation Palette (8 hex colors required)
[colors]
background = "#0d0f18"
surface    = "#181b28"
text       = "#f0f6fc"
muted      = "#8b949e"
accent     = "#00f0ff"
success    = "#00ff88"
warning    = "#ffe600"
error      = "#ff0055"

# 2. Component Token Overrides (optional)
[components.input]
bg_active   = "#222638"
bg_inactive = "#141622"
caret       = "#00f0ff"
selection   = "#333852"
placeholder = "#6e7681"

[components.crate]
fg       = "#ff00a0"
badge_bg = "#2a152e"

[components.diff]
add_bg = "#0d2b1a"
del_bg = "#2b0d18"
add_hl = "#1b5433"
del_hl = "#541b30"

[components.command]
idle_bg  = "#1a1d2e"
hover_bg = "#252940"
```

### Loading & Discovery

1. Custom theme files placed in `~/.config/mutx/themes/` are dynamically parsed on launch.
2. The stem of the filename (e.g. `cyberpunk.toml` → `cyberpunk`) becomes the unique theme ID.
3. Themes appear automatically in the full-screen Settings View (`/config` › Appearance) with live transcript and component swatches.
4. Themes can be selected directly in the UI or specified in `~/.config/mutx/config.toml`:

```toml
color_scheme = "cyberpunk"
```
