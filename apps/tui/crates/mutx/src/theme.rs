//! Color palette used across the renderer.

use std::borrow::Cow;

use muta_contracts::{ColorSchemeConfig, ComponentThemesConfig, ThemeFile};
use mutx_engine::Color;

/// Metadata for one color scheme shown by the Appearance config page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ColorSchemePreset {
    pub id: Cow<'static, str>,
    pub label: Cow<'static, str>,
    pub description: Cow<'static, str>,
    pub custom: bool,
    pub is_file: bool,
}

impl ColorSchemePreset {
    pub const fn static_preset(
        id: &'static str,
        label: &'static str,
        description: &'static str,
        custom: bool,
    ) -> Self {
        Self {
            id: Cow::Borrowed(id),
            label: Cow::Borrowed(label),
            description: Cow::Borrowed(description),
            custom,
            is_file: false,
        }
    }
}

/// Built-in palettes plus the editable custom slot. Order is the UI order.
pub const COLOR_SCHEMES: [ColorSchemePreset; 6] = [
    ColorSchemePreset::static_preset("zen", "Zen", "Quiet charcoal with sage accents", false),
    ColorSchemePreset::static_preset(
        "midnight",
        "Midnight",
        "Deep navy with crisp blue accents",
        false,
    ),
    ColorSchemePreset::static_preset("nord", "Nord", "Cool arctic blues and soft contrast", false),
    ColorSchemePreset::static_preset(
        "catppuccin",
        "Catppuccin",
        "Warm mocha with lavender accents",
        false,
    ),
    ColorSchemePreset::static_preset(
        "paper",
        "Paper",
        "Warm light surface for bright terminals",
        false,
    ),
    ColorSchemePreset::static_preset(
        "custom",
        "Custom",
        "Your editable eight-color palette",
        true,
    ),
];

// ── Default Styling Constants ──────────────────────────────────────────────
pub const DEFAULT_CRATE_FG: Color = Color::Rgb(180, 190, 254);
pub const DEFAULT_CARET_FG: Color = Color::Rgb(213, 213, 205);
pub const DEFAULT_INPUT_PLACEHOLDER_FG: Color = Color::Rgb(119, 125, 117);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SemanticPalette {
    pub background: Color,
    pub surface: Color,
    pub text: Color,
    pub muted: Color,
    pub accent: Color,
    pub success: Color,
    pub warning: Color,
    pub error: Color,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CustomColorField {
    pub label: &'static str,
    pub hint: &'static str,
}

pub const CUSTOM_COLOR_FIELDS: [CustomColorField; 8] = [
    CustomColorField {
        label: "Background",
        hint: "terminal canvas",
    },
    CustomColorField {
        label: "Surface",
        hint: "panels and menus",
    },
    CustomColorField {
        label: "Text",
        hint: "primary foreground",
    },
    CustomColorField {
        label: "Muted",
        hint: "secondary foreground",
    },
    CustomColorField {
        label: "Accent",
        hint: "focus and brand",
    },
    CustomColorField {
        label: "Success",
        hint: "positive states",
    },
    CustomColorField {
        label: "Warning",
        hint: "caution states",
    },
    CustomColorField {
        label: "Error",
        hint: "failure states",
    },
];

/// Styles used during rendering.
#[derive(Clone)]
pub struct Theme {
    pub user_fg: Color,
    pub error_fg: Color,
    pub system_fg: Color,
    pub code_fg: Color,
    pub code_bg: Color,
    pub heading_fg: Color,
    pub quote_fg: Color,
    pub dim_fg: Color,
    pub selected_bg: Color,
    // opencode-style semantic design tokens.
    /// Base background painted across the entire terminal frame so the TUI
    /// owns every pixel rather than relying on the terminal emulator default.
    pub app_bg: Color,
    /// Primary foreground text.
    pub text: Color,
    /// Muted/secondary text.
    pub text_muted: Color,
    /// Intermediate foreground for an interactive step header that is under
    /// the pointer but not in its expanded/active state — sits between
    /// `text_muted` (idle) and `text` (expanded) so hover reads as a softer
    /// affordance than "open".
    pub text_hover: Color,
    /// Solid background for panels (modals, sheets).
    pub panel_bg: Color,
    /// Background for the live input box while it owns the keyboard — the
    /// brightest interactive surface, so "I am typing here" reads from
    /// luminance alone. Input-specific pair token 1 of 2: the input box owns
    /// both values outright (they are deliberately *not* derived from or
    /// shared with the user-message panel tokens), so retuning the input
    /// never ripples into other surfaces and vice versa.
    pub input_bg_active: Color,
    /// Background for the input box while it does **not** own the keyboard
    /// (a transcript step is focused) — a recessed, inert band between
    /// `app_bg` and `panel_bg`. Input-specific pair token 2 of 2; see
    /// [`Theme::input_bg_active`]. The active↔inactive step is large enough
    /// that each state is distinguishable both from the app background and
    /// from the other state.
    pub input_bg_inactive: Color,
    /// Used for sent user messages so they read as read-only compared to the
    /// live input box.
    pub user_panel_bg: Color,
    /// Background for user messages staged in the send queue (waiting for
    /// the in-flight turn to finish). Dimmer than `user_panel_bg` so a
    /// queued message reads as more "pending" than a delivered one without
    /// losing the panel affordance.
    pub user_panel_bg_queued: Color,
    /// Slightly raised background for footer/option bars.
    pub element_bg: Color,
    /// Background for menus / suggestion popups.
    pub menu_bg: Color,
    /// Dim overlay drawn behind modals to fake alpha.
    pub backdrop: Color,
    /// Brightness multiplier (0.0–1.0) applied to every cell of the live
    /// surface while a [`Recess::Dim`](crate::modal::Recess) modal is open.
    /// The terminal cannot alpha-blend, so a dim-recess modal darkens the
    /// transcript/chrome in place by scaling each color by this factor — lower
    /// is darker. This is the single knob for how strongly an open modal
    /// recedes the background for focus.
    pub modal_dim_factor: f32,
    /// Brand / selection color.
    pub primary: Color,
    pub warning: Color,
    pub success: Color,
    pub info: Color,
    /// Diff banding. Every block-level code/text surface shares one
    /// design contract (see the disclosure module): colors flow through
    /// theme tokens rather than magic `Color::Rgb` literals, so retuning
    /// the palette in one place retunes every block. The diff block is
    /// the reference renderer — it owns the row/highlight pair — and the
    /// flat code blocks (read / bash / listing / matches / markdown) reuse
    /// the same token system via [`code_surface`](Theme::code_surface).
    /// Low-chroma row tint so added/removed blocks read at a glance.
    pub diff_add_bg: Color,
    pub diff_del_bg: Color,
    /// Brighter per-word highlight tint layered on top of the row band;
    /// the exact edited word sits on this brighter surface.
    pub diff_add_hl: Color,
    pub diff_del_hl: Color,
    /// Command-card band (ADR-0109). A slash/shell command row is a *card*,
    /// not flat prose: `body_surface` is its idle band, `body_hover` the
    /// band while the row is under the pointer or keyboard focus — the same
    /// element_bg → input_bg_active hover ladder the notice card uses. The
    /// pair is derived (not two more literals) so every scheme gets a
    /// coherent step for free; see [`Theme::command_surface`].
    pub command_band_bg: Color,
    pub command_band_bg_hover: Color,

    // Component-specific tokens
    pub caret_fg: Color,
    pub input_selection_bg: Color,
    pub input_placeholder_fg: Color,
    pub crate_fg: Color,
    pub crate_bg: Color,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            user_fg: Color::Rgb(165, 177, 164),
            error_fg: Color::Rgb(190, 111, 104),
            system_fg: Color::Rgb(111, 116, 110),
            code_fg: Color::Rgb(166, 178, 163),
            code_bg: Color::Rgb(17, 19, 18),
            heading_fg: Color::Rgb(190, 194, 181),
            quote_fg: Color::Rgb(156, 145, 118),
            dim_fg: Color::Rgb(94, 99, 94),
            selected_bg: Color::Rgb(38, 48, 44),
            // Zen palette: ink-black base, charcoal surfaces, quiet sage
            // accents. Contrast comes from luminance, not saturated hue, so
            // the interface stays calm while preserving semantic cues.
            app_bg: Color::Rgb(7, 8, 8),
            text: Color::Rgb(213, 213, 205),
            text_muted: Color::Rgb(119, 125, 117),
            text_hover: Color::Rgb(175, 180, 172),
            panel_bg: Color::Rgb(14, 15, 15),
            input_bg_active: Color::Rgb(26, 28, 27),
            input_bg_inactive: Color::Rgb(16, 17, 17),
            user_panel_bg: Color::Rgb(17, 22, 19),
            user_panel_bg_queued: Color::Rgb(9, 12, 11),
            element_bg: Color::Rgb(21, 23, 22),
            menu_bg: Color::Rgb(17, 19, 18),
            backdrop: Color::Rgb(3, 4, 4),
            modal_dim_factor: 0.5,
            primary: Color::Rgb(142, 161, 145),
            warning: Color::Rgb(181, 149, 93),
            success: Color::Rgb(117, 148, 117),
            info: Color::Rgb(128, 153, 156),
            diff_add_bg: Color::Rgb(18, 31, 22),
            diff_del_bg: Color::Rgb(32, 20, 20),
            diff_add_hl: Color::Rgb(42, 64, 48),
            diff_del_hl: Color::Rgb(64, 40, 40),
            command_band_bg: Color::Rgb(17, 19, 18),
            command_band_bg_hover: Color::Rgb(26, 28, 27),

            caret_fg: DEFAULT_CARET_FG,
            input_selection_bg: Color::Rgb(38, 48, 44),
            input_placeholder_fg: DEFAULT_INPUT_PLACEHOLDER_FG,
            crate_fg: DEFAULT_CRATE_FG,
            crate_bg: Color::Rgb(25, 27, 34),
        }
    }
}

/// Semantic accessors (ADR-0001 P4): renderers reference intent
/// (surface / body / raised / ok / err / …) rather than the raw palette field
/// names, so the palette can be retuned in one place. The fields stay `pub`
/// for `Theme::default()` construction; new rendering code should prefer these.
impl Theme {
    /// Return all available color schemes: built-ins + custom theme files in `$XDG_CONFIG_HOME/mutx/themes` + custom slot.
    pub fn available_color_schemes() -> Vec<ColorSchemePreset> {
        let mut list = Vec::new();
        // 1. Built-in presets (Zen, Midnight, Nord, Catppuccin, Paper)
        for preset in COLOR_SCHEMES.iter().take(5) {
            list.push(preset.clone());
        }
        // 2. Custom files from mutx themes_dir (and legacy muta themes_dir)
        let themes_dir = crate::paths::get().themes_dir();
        let mut files = crate::config::load_theme_files(&themes_dir);
        let legacy_themes_dir = muta_persistence::paths::get().themes_dir();
        if legacy_themes_dir.exists() && legacy_themes_dir != themes_dir {
            files.extend(crate::config::load_theme_files(&legacy_themes_dir));
        }
        for file in files {
            if !list
                .iter()
                .any(|item| item.id.eq_ignore_ascii_case(&file.id))
                && !file.id.eq_ignore_ascii_case("custom")
            {
                list.push(ColorSchemePreset {
                    id: Cow::Owned(file.id),
                    label: Cow::Owned(file.name),
                    description: Cow::Owned(file.description),
                    custom: false,
                    is_file: true,
                });
            }
        }
        // 3. The Custom editor slot
        list.push(COLOR_SCHEMES[5].clone());
        list
    }

    /// Canonicalize a persisted scheme id. Unknown and empty ids use Zen.
    pub fn normalize_color_scheme(name: &str) -> String {
        let name = name.trim();
        let schemes = Self::available_color_schemes();
        schemes
            .iter()
            .find(|scheme| scheme.id.eq_ignore_ascii_case(name))
            .map(|scheme| scheme.id.to_string())
            .unwrap_or_else(|| "zen".to_string())
    }

    pub fn color_scheme_index(name: &str) -> usize {
        let name = name.trim();
        let schemes = Self::available_color_schemes();
        schemes
            .iter()
            .position(|scheme| scheme.id.eq_ignore_ascii_case(name))
            .unwrap_or(0)
    }

    pub fn color_scheme_label(name: &str) -> String {
        let name = name.trim();
        let schemes = Self::available_color_schemes();
        schemes
            .iter()
            .find(|scheme| scheme.id.eq_ignore_ascii_case(name))
            .map(|scheme| scheme.label.to_string())
            .unwrap_or_else(|| "Zen".to_string())
    }

    /// Build a complete renderer theme from a preset id, external theme file, or custom semantics.
    pub fn from_color_scheme(name: &str, custom: &ColorSchemeConfig) -> Self {
        match name.trim().to_ascii_lowercase().as_str() {
            "midnight" => Self::from_semantic(SemanticPalette {
                background: Color::Rgb(6, 10, 18),
                surface: Color::Rgb(14, 20, 32),
                text: Color::Rgb(218, 225, 240),
                muted: Color::Rgb(112, 124, 148),
                accent: Color::Rgb(91, 156, 255),
                success: Color::Rgb(87, 190, 141),
                warning: Color::Rgb(226, 174, 91),
                error: Color::Rgb(226, 105, 117),
            }),
            "nord" => Self::from_semantic(SemanticPalette {
                background: Color::Rgb(46, 52, 64),
                surface: Color::Rgb(59, 66, 82),
                text: Color::Rgb(236, 239, 244),
                muted: Color::Rgb(136, 148, 166),
                accent: Color::Rgb(136, 192, 208),
                success: Color::Rgb(163, 190, 140),
                warning: Color::Rgb(235, 203, 139),
                error: Color::Rgb(191, 97, 106),
            }),
            "catppuccin" => Self::from_semantic(SemanticPalette {
                background: Color::Rgb(30, 30, 46),
                surface: Color::Rgb(49, 50, 68),
                text: Color::Rgb(205, 214, 244),
                muted: Color::Rgb(147, 153, 178),
                accent: Color::Rgb(203, 166, 247),
                success: Color::Rgb(166, 227, 161),
                warning: Color::Rgb(249, 226, 175),
                error: Color::Rgb(243, 139, 168),
            }),
            "paper" => Self::from_semantic(SemanticPalette {
                background: Color::Rgb(247, 246, 242),
                surface: Color::Rgb(255, 255, 252),
                text: Color::Rgb(43, 45, 48),
                muted: Color::Rgb(106, 110, 114),
                accent: Color::Rgb(55, 100, 165),
                success: Color::Rgb(56, 126, 79),
                warning: Color::Rgb(157, 105, 31),
                error: Color::Rgb(177, 63, 58),
            }),
            "custom" => Self::from_custom(custom),
            "zen" => Self::default(),
            other => {
                let themes_dir = crate::paths::get().themes_dir();
                let mut files = crate::config::load_theme_files(&themes_dir);
                let legacy_themes_dir = muta_persistence::paths::get().themes_dir();
                if legacy_themes_dir.exists() && legacy_themes_dir != themes_dir {
                    files.extend(crate::config::load_theme_files(&legacy_themes_dir));
                }
                if let Some(found) = files.into_iter().find(|t| t.id.eq_ignore_ascii_case(other)) {
                    Self::from_theme_file(&found)
                } else {
                    Self::default()
                }
            }
        }
    }

    /// Build a complete renderer theme from a parsed [`ThemeFile`].
    pub fn from_theme_file(file: &ThemeFile) -> Self {
        let mut theme = Self::from_custom(&file.colors);
        theme.apply_component_overrides(&file.components);
        theme
    }

    /// Apply component-specific overrides onto an existing theme.
    pub fn apply_component_overrides(&mut self, overrides: &Option<ComponentThemesConfig>) {
        let Some(components) = overrides else { return };
        if let Some(ref input) = components.input {
            if let Some(val) = input.bg_active.as_deref().and_then(Self::color_from_hex) {
                self.input_bg_active = val;
            }
            if let Some(val) = input.bg_inactive.as_deref().and_then(Self::color_from_hex) {
                self.input_bg_inactive = val;
            }
            if let Some(val) = input.caret.as_deref().and_then(Self::color_from_hex) {
                self.caret_fg = val;
            }
            if let Some(val) = input.selection.as_deref().and_then(Self::color_from_hex) {
                self.input_selection_bg = val;
            }
            if let Some(val) = input.placeholder.as_deref().and_then(Self::color_from_hex) {
                self.input_placeholder_fg = val;
            }
        }
        if let Some(ref crate_c) = components.crate_component {
            if let Some(val) = crate_c.fg.as_deref().and_then(Self::color_from_hex) {
                self.crate_fg = val;
            }
            if let Some(val) = crate_c.badge_bg.as_deref().and_then(Self::color_from_hex) {
                self.crate_bg = val;
            }
        }
        if let Some(ref diff) = components.diff {
            if let Some(val) = diff.add_bg.as_deref().and_then(Self::color_from_hex) {
                self.diff_add_bg = val;
            }
            if let Some(val) = diff.del_bg.as_deref().and_then(Self::color_from_hex) {
                self.diff_del_bg = val;
            }
            if let Some(val) = diff.add_hl.as_deref().and_then(Self::color_from_hex) {
                self.diff_add_hl = val;
            }
            if let Some(val) = diff.del_hl.as_deref().and_then(Self::color_from_hex) {
                self.diff_del_hl = val;
            }
        }
        if let Some(ref command) = components.command {
            if let Some(val) = command.idle_bg.as_deref().and_then(Self::color_from_hex) {
                self.command_band_bg = val;
            }
            if let Some(val) = command.hover_bg.as_deref().and_then(Self::color_from_hex) {
                self.command_band_bg_hover = val;
            }
        }
    }

    /// Colors used by the compact scheme preview in `/config`.
    pub fn preview_colors(name: &str, custom: &ColorSchemeConfig) -> [Color; 5] {
        let theme = Self::from_color_scheme(name, custom);
        [
            theme.surface(),
            theme.panel(),
            theme.brand(),
            theme.ok(),
            theme.warn(),
        ]
    }

    pub fn custom_color_value(config: &ColorSchemeConfig, index: usize) -> Option<&str> {
        match index {
            0 => Some(&config.background),
            1 => Some(&config.surface),
            2 => Some(&config.text),
            3 => Some(&config.muted),
            4 => Some(&config.accent),
            5 => Some(&config.success),
            6 => Some(&config.warning),
            7 => Some(&config.error),
            _ => None,
        }
    }

    /// Store one custom field after canonical `#RRGGBB` validation.
    pub fn set_custom_color_value(
        config: &mut ColorSchemeConfig,
        index: usize,
        value: &str,
    ) -> bool {
        let Some(value) = normalize_hex(value) else {
            return false;
        };
        match index {
            0 => config.background = value,
            1 => config.surface = value,
            2 => config.text = value,
            3 => config.muted = value,
            4 => config.accent = value,
            5 => config.success = value,
            6 => config.warning = value,
            7 => config.error = value,
            _ => return false,
        }
        true
    }

    pub fn color_from_hex(value: &str) -> Option<Color> {
        let value = value.strip_prefix('#').unwrap_or(value);
        if value.len() != 6 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return None;
        }
        let r = u8::from_str_radix(&value[0..2], 16).ok()?;
        let g = u8::from_str_radix(&value[2..4], 16).ok()?;
        let b = u8::from_str_radix(&value[4..6], 16).ok()?;
        Some(Color::Rgb(r, g, b))
    }

    fn from_custom(custom: &ColorSchemeConfig) -> Self {
        let fallback = ColorSchemeConfig::default();
        let color = |value: &str, fallback_value: &str| {
            Self::color_from_hex(value)
                .or_else(|| Self::color_from_hex(fallback_value))
                .unwrap_or(Color::Black)
        };
        Self::from_semantic(SemanticPalette {
            background: color(&custom.background, &fallback.background),
            surface: color(&custom.surface, &fallback.surface),
            text: color(&custom.text, &fallback.text),
            muted: color(&custom.muted, &fallback.muted),
            accent: color(&custom.accent, &fallback.accent),
            success: color(&custom.success, &fallback.success),
            warning: color(&custom.warning, &fallback.warning),
            error: color(&custom.error, &fallback.error),
        })
    }

    fn from_semantic(palette: SemanticPalette) -> Self {
        let SemanticPalette {
            background,
            surface,
            text,
            muted,
            accent,
            success,
            warning,
            error,
        } = palette;
        let light = luminance(background) > 150.0;
        let code_bg = mix(background, text, if light { 0.06 } else { 0.05 });
        let user_bg = mix(surface, accent, if light { 0.08 } else { 0.10 });
        let menu_bg = mix(background, surface, 0.72);
        Self {
            user_fg: mix(text, accent, 0.18),
            error_fg: error,
            system_fg: muted,
            code_fg: mix(text, accent, 0.10),
            code_bg,
            heading_fg: text,
            quote_fg: warning,
            dim_fg: mix(background, muted, if light { 0.82 } else { 0.70 }),
            selected_bg: mix(surface, accent, if light { 0.18 } else { 0.24 }),
            app_bg: background,
            text,
            text_muted: muted,
            text_hover: mix(muted, text, 0.55),
            panel_bg: surface,
            // The input pair derives from `panel_bg`, keeping the two states
            // related (same hue family, two distinct luminance steps) while
            // staying input-owned: inactive sits at the panel's own level of
            // elevation, active is lifted well clear of every other surface
            // token so the focused box cannot be confused with the ambient
            // chrome (element_bg sits at ~5% toward text; active goes to 8%).
            input_bg_active: mix(surface, text, if light { 0.06 } else { 0.08 }),
            input_bg_inactive: mix(surface, text, if light { 0.015 } else { 0.02 }),
            user_panel_bg: user_bg,
            user_panel_bg_queued: mix(background, user_bg, 0.45),
            element_bg: mix(surface, text, if light { 0.035 } else { 0.05 }),
            menu_bg,
            backdrop: mix(background, Color::Black, if light { 0.45 } else { 0.55 }),
            modal_dim_factor: if light { 0.72 } else { 0.5 },
            primary: accent,
            warning,
            success,
            info: mix(accent, text, 0.12),
            diff_add_bg: mix(background, success, if light { 0.11 } else { 0.15 }),
            diff_del_bg: mix(background, error, if light { 0.10 } else { 0.14 }),
            diff_add_hl: mix(background, success, if light { 0.24 } else { 0.34 }),
            diff_del_hl: mix(background, error, if light { 0.22 } else { 0.32 }),
            // Command card (ADR-0109): derive the pair from the body/raised
            // surfaces so the card inherits each scheme's elevation ladder
            // rather than a fixed literal. Idle is the menu surface lifted a
            // hair off the page; hover is the same scheme-wide active tone
            // (`input_bg_active`) the notice card hovers to, so "an
            // interactive card is lit up" reads identically everywhere.
            command_band_bg: mix(background, surface, 0.72),
            command_band_bg_hover: mix(surface, text, if light { 0.06 } else { 0.08 }),
            caret_fg: text,
            input_selection_bg: mix(surface, accent, if light { 0.18 } else { 0.24 }),
            input_placeholder_fg: muted,
            crate_fg: mix(accent, Color::Rgb(180, 190, 254), 0.65),
            crate_bg: mix(background, accent, if light { 0.08 } else { 0.12 }),
        }
    }

    // ── Surfaces (backgrounds) ──
    /// Frame background — the base everything sits on.
    pub fn surface(&self) -> Color {
        self.app_bg
    }
    /// Step body / content surface.
    pub fn body(&self) -> Color {
        self.menu_bg
    }
    /// Raised surface (header bands, footer bars).
    pub fn raised(&self) -> Color {
        self.element_bg
    }
    /// Modal / sheet surface.
    pub fn panel(&self) -> Color {
        self.panel_bg
    }
    /// Live input-box surface while the box owns the keyboard.
    pub fn input_surface(&self) -> Color {
        self.input_bg_active
    }
    /// Live input-box surface while a transcript step owns the keyboard and
    /// the box is inert.
    pub fn input_surface_inactive(&self) -> Color {
        self.input_bg_inactive
    }
    /// Sent-user-message surface.
    pub fn user_surface(&self) -> Color {
        self.user_panel_bg
    }
    /// Surface for a user message staged in the send queue. Dimmer than
    /// [`Theme::user_surface`] so pending reads differently from delivered.
    pub fn user_surface_queued(&self) -> Color {
        self.user_panel_bg_queued
    }
    /// Dim overlay behind modals.
    pub fn backdrop(&self) -> Color {
        self.backdrop
    }
    /// Brightness factor (0.0–1.0) the dim-recess pass scales the live surface
    /// by. Lower is darker. See [`Theme::modal_dim_factor`](struct.Theme.html#structfield.modal_dim_factor).
    pub fn modal_dim_factor(&self) -> f32 {
        self.modal_dim_factor
    }
    /// Selection highlight background.
    pub fn selected(&self) -> Color {
        self.selected_bg
    }

    // ── Foregrounds ──
    pub fn fg(&self) -> Color {
        self.text
    }
    pub fn muted(&self) -> Color {
        self.text_muted
    }
    /// Foreground for an interactive step header while collapsed but under the
    /// pointer — an intermediate tone between `muted()` (idle) and `fg()`
    /// (expanded/active), so hover reads as a softer affordance than "open".
    pub fn hover(&self) -> Color {
        self.text_hover
    }
    pub fn dim(&self) -> Color {
        self.dim_fg
    }
    pub fn brand(&self) -> Color {
        self.primary
    }
    pub fn ok(&self) -> Color {
        self.success
    }
    pub fn warn(&self) -> Color {
        self.warning
    }
    pub fn err(&self) -> Color {
        self.error_fg
    }
    pub fn info(&self) -> Color {
        self.info
    }

    // ── Attachment chips ──
    // Paste chips (`[Pasted text #N +M lines (size)]`) and image chips
    // (`[Image #N (size)]`) are the composer's identifiers for staged
    // attachments. Each kind gets its own foreground + tinted band so pasted
    // blocks read as distinct typed objects inside the live input — the blue
    // marks "text block", the amber marks "image block". Both derive from
    // existing palette tokens so every color scheme gets a coherent pair.
    /// Foreground of a staged large-text-paste chip. A calm blue reads as
    /// "document / content block" — distinct from plain prose and from the
    /// brand-colored `/command` accent.
    pub fn chip_paste_fg(&self) -> Color {
        self.info
    }
    /// Tinted pill background behind a paste chip. Derived from the surface
    /// the chip sits on (the composer panel) so the band reads on both the
    /// focused and the blurred input box.
    pub fn chip_paste_bg(&self, on: Color) -> Color {
        mix(on, self.info, 0.22)
    }
    /// Foreground of a staged image chip. A warm amber reads as "media
    /// attachment", clearly distinguishable from the text-block blue.
    pub fn chip_image_fg(&self) -> Color {
        self.warning
    }
    /// Tinted pill background behind an image chip (see
    /// [`Theme::chip_paste_bg`]).
    pub fn chip_image_bg(&self, on: Color) -> Color {
        mix(on, self.warning, 0.18)
    }
    pub fn code_text(&self) -> Color {
        self.code_fg
    }
    pub fn code_surface(&self) -> Color {
        self.code_bg
    }
    /// Diff block row band — the low-chroma tint a whole added line sits on.
    /// The reference block-level renderer's colors are first-class tokens so
    /// every block-level surface shares one palette contract.
    pub fn diff_add_bg(&self) -> Color {
        self.diff_add_bg
    }
    /// Diff block row band for a whole removed line.
    pub fn diff_del_bg(&self) -> Color {
        self.diff_del_bg
    }
    /// Diff block per-word highlight on an added line (brighter than the row band).
    pub fn diff_add_hl(&self) -> Color {
        self.diff_add_hl
    }
    /// Diff block per-word highlight on a removed line (brighter than the row band).
    pub fn diff_del_hl(&self) -> Color {
        self.diff_del_hl
    }
    /// Command-card band (ADR-0109): the full-width surface a command row
    /// paints its card on while idle. Distinct from the page background in
    /// every scheme so a command never reads as flat prose.
    pub fn command_surface(&self) -> Color {
        self.command_band_bg
    }
    /// Command-card band while the row is hovered or keyboard-focused —
    /// the shared "interactive card lit up" tone.
    pub fn command_surface_hover(&self) -> Color {
        self.command_band_bg_hover
    }
    pub fn heading(&self) -> Color {
        self.heading_fg
    }
    pub fn quote(&self) -> Color {
        self.quote_fg
    }
    pub fn user_text(&self) -> Color {
        self.user_fg
    }
    pub fn system_text(&self) -> Color {
        self.system_fg
    }
    /// Caret / cursor insertion point color.
    pub fn caret(&self) -> Color {
        self.caret_fg
    }
    /// Input placeholder text color.
    pub fn input_placeholder(&self) -> Color {
        self.input_placeholder_fg
    }
    /// Input selection highlight color.
    pub fn input_selection(&self) -> Color {
        self.input_selection_bg
    }
    /// Crate identifier / tag foreground color.
    pub fn crate_tag(&self) -> Color {
        self.crate_fg
    }
    /// Crate badge background tint.
    pub fn crate_badge(&self) -> Color {
        self.crate_bg
    }
}

fn normalize_hex(value: &str) -> Option<String> {
    let value = value.trim();
    let digits = value.strip_prefix('#').unwrap_or(value);
    if digits.len() == 6 && digits.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Some(format!("#{}", digits.to_ascii_lowercase()))
    } else {
        None
    }
}

fn rgb(color: Color) -> (u8, u8, u8) {
    match color {
        Color::Rgb(r, g, b) => (r, g, b),
        Color::Black => (0, 0, 0),
        Color::White => (255, 255, 255),
        _ => (128, 128, 128),
    }
}

fn mix(a: Color, b: Color, amount: f32) -> Color {
    let amount = amount.clamp(0.0, 1.0);
    let (ar, ag, ab) = rgb(a);
    let (br, bg, bb) = rgb(b);
    let channel =
        |left: u8, right: u8| (left as f32 + (right as f32 - left as f32) * amount).round() as u8;
    Color::Rgb(channel(ar, br), channel(ag, bg), channel(ab, bb))
}

fn luminance(color: Color) -> f32 {
    let (r, g, b) = rgb(color);
    0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_scheme_falls_back_to_zen() {
        let custom = ColorSchemeConfig::default();
        let fallback = Theme::from_color_scheme("not-a-theme", &custom);
        assert_eq!(fallback.surface(), Theme::default().surface());
        assert_eq!(Theme::normalize_color_scheme(""), "zen");
    }

    #[test]
    fn custom_hex_values_are_canonicalized_and_applied() {
        let mut custom = ColorSchemeConfig::default();
        assert!(Theme::set_custom_color_value(&mut custom, 4, "A1B2c3"));
        assert_eq!(custom.accent, "#a1b2c3");
        assert!(!Theme::set_custom_color_value(&mut custom, 4, "blue"));
        assert_eq!(
            Theme::from_color_scheme("custom", &custom).brand(),
            Color::Rgb(161, 178, 195)
        );
    }

    #[test]
    fn every_preset_has_a_distinct_canonical_index() {
        let schemes = Theme::available_color_schemes();
        for (index, scheme) in schemes.iter().enumerate() {
            assert_eq!(Theme::color_scheme_index(&scheme.id), index);
        }
    }

    #[test]
    fn component_overrides_apply_correctly() {
        let raw = r##"
name = "Cyberpunk"
description = "Neon high-contrast"
[colors]
background = "#050505"
surface = "#151515"
text = "#ffffff"
muted = "#808080"
accent = "#00ffff"
success = "#00ff00"
warning = "#ffff00"
error = "#ff0055"

[components.input]
bg_active = "#2a2a2a"
bg_inactive = "#111111"
caret = "#ff00ff"
selection = "#333333"
placeholder = "#777777"

[components.crate]
fg = "#00ffff"
badge_bg = "#1a3333"

[components.diff]
add_bg = "#003300"
del_bg = "#330011"
add_hl = "#006600"
del_hl = "#660022"

[components.command]
idle_bg = "#181818"
hover_bg = "#282828"
"##;
        let file: ThemeFile = toml::from_str(raw).expect("should parse");
        let theme = Theme::from_theme_file(&file);
        assert_eq!(theme.input_surface(), Color::Rgb(42, 42, 42));
        assert_eq!(theme.input_surface_inactive(), Color::Rgb(17, 17, 17));
        assert_eq!(theme.caret(), Color::Rgb(255, 0, 255));
        assert_eq!(theme.input_selection(), Color::Rgb(51, 51, 51));
        assert_eq!(theme.input_placeholder(), Color::Rgb(119, 119, 119));
        assert_eq!(theme.crate_tag(), Color::Rgb(0, 255, 255));
        assert_eq!(theme.crate_badge(), Color::Rgb(26, 51, 51));
        assert_eq!(theme.diff_add_bg(), Color::Rgb(0, 51, 0));
        assert_eq!(theme.diff_del_bg(), Color::Rgb(51, 0, 17));
        assert_eq!(theme.diff_add_hl(), Color::Rgb(0, 102, 0));
        assert_eq!(theme.diff_del_hl(), Color::Rgb(102, 0, 34));
        assert_eq!(theme.command_surface(), Color::Rgb(24, 24, 24));
        assert_eq!(theme.command_surface_hover(), Color::Rgb(40, 40, 40));
    }

    /// The input box owns two dedicated background tokens (independent of the
    /// other surface tokens) and the active/inactive pair must stay
    /// distinguishable in *every* scheme: each state must clear the app
    /// background by a visible margin, and the two states must clear each
    /// other by a visible margin. This is the regression guard for the
    /// "activated and deactivated input look identical to the background"
    /// defect that motivated the pair.
    #[test]
    fn input_surfaces_stay_distinguishable_in_every_scheme() {
        const MIN_STEP: f32 = 4.0;
        let custom = ColorSchemeConfig::default();
        for scheme in &COLOR_SCHEMES {
            let theme = Theme::from_color_scheme(&scheme.id, &custom);
            let active = theme.input_surface();
            let inactive = theme.input_surface_inactive();
            let base = theme.surface();

            // Light schemes mix toward a dark text (surfaces get *darker* as
            // they elevate), so the margin is measured as an absolute delta.
            let step_from_app_active = (luminance(active) - luminance(base)).abs();
            let step_from_app_inactive = (luminance(inactive) - luminance(base)).abs();
            let step_between_states = (luminance(active) - luminance(inactive)).abs();

            assert!(
                step_from_app_active >= MIN_STEP,
                "{scheme:?}: active input only {step_from_app_active:.1} above app_bg"
            );
            assert!(
                step_from_app_inactive >= MIN_STEP,
                "{scheme:?}: inactive input only {step_from_app_inactive:.1} above app_bg"
            );
            assert!(
                step_between_states >= MIN_STEP,
                "{scheme:?}: active/inactive input only {step_between_states:.1} apart"
            );
            assert_ne!(
                active, inactive,
                "{scheme:?}: active and inactive input must be distinct colors"
            );
        }
    }

    /// ADR-0109: the command card must read as a *card* in every scheme —
    /// both its idle band and its hover band have to clear the page
    /// background by a visible margin (otherwise the row collapses back
    /// into "flat prose", the exact defect the card exists to fix), and the
    /// hover step must itself be visible. Derived for every preset via
    /// `from_semantic`, asserted here so a future scheme cannot silently
    /// flatten it.
    #[test]
    fn command_card_bands_stay_visible_in_every_scheme() {
        const MIN_STEP: f32 = 2.0;
        let custom = ColorSchemeConfig::default();
        for scheme in &COLOR_SCHEMES {
            let theme = Theme::from_color_scheme(&scheme.id, &custom);
            let idle = theme.command_surface();
            let hover = theme.command_surface_hover();
            let base = theme.surface();

            let idle_step = (luminance(idle) - luminance(base)).abs();
            let hover_step = (luminance(hover) - luminance(base)).abs();
            let hover_idle_step = (luminance(hover) - luminance(idle)).abs();

            assert!(
                idle_step >= MIN_STEP,
                "{scheme:?}: idle command band only {idle_step:.1} from app_bg"
            );
            assert!(
                hover_step >= MIN_STEP,
                "{scheme:?}: hover command band only {hover_step:.1} from app_bg"
            );
            assert!(
                hover_idle_step >= MIN_STEP,
                "{scheme:?}: command hover only {hover_idle_step:.1} above its idle band"
            );
        }
    }
}
