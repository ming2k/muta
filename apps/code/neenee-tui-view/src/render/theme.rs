//! Color palette used across the renderer.

use neenee_core::ColorSchemeConfig;
use neenee_tui::Color;

/// Metadata for one color scheme shown by the Appearance config page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ColorSchemePreset {
    pub id: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    pub custom: bool,
}

/// Built-in palettes plus the editable custom slot. Order is the UI order.
pub const COLOR_SCHEMES: [ColorSchemePreset; 6] = [
    ColorSchemePreset {
        id: "zen",
        label: "Zen",
        description: "Quiet charcoal with sage accents",
        custom: false,
    },
    ColorSchemePreset {
        id: "midnight",
        label: "Midnight",
        description: "Deep navy with crisp blue accents",
        custom: false,
    },
    ColorSchemePreset {
        id: "nord",
        label: "Nord",
        description: "Cool arctic blues and soft contrast",
        custom: false,
    },
    ColorSchemePreset {
        id: "catppuccin",
        label: "Catppuccin",
        description: "Warm mocha with lavender accents",
        custom: false,
    },
    ColorSchemePreset {
        id: "paper",
        label: "Paper",
        description: "Warm light surface for bright terminals",
        custom: false,
    },
    ColorSchemePreset {
        id: "custom",
        label: "Custom",
        description: "Your editable eight-color palette",
        custom: true,
    },
];

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
    /// Background for the live input box; brighter than `user_panel_bg` so the
    /// active prompt stands out from already-sent messages.
    pub input_bg: Color,
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
    /// flat code blocks (read / bash / listing / grep / markdown) reuse
    /// the same token system via [`code_surface`](Theme::code_surface).
    /// Low-chroma row tint so added/removed blocks read at a glance.
    pub diff_add_bg: Color,
    pub diff_del_bg: Color,
    /// Brighter per-word highlight tint layered on top of the row band;
    /// the exact edited word sits on this brighter surface.
    pub diff_add_hl: Color,
    pub diff_del_hl: Color,
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
            input_bg: Color::Rgb(18, 19, 19),
            user_panel_bg: Color::Rgb(17, 22, 19),
            user_panel_bg_queued: Color::Rgb(9, 12, 11),
            element_bg: Color::Rgb(21, 23, 22),
            menu_bg: Color::Rgb(17, 19, 18),
            backdrop: Color::Rgb(3, 4, 4),
            // Halves surface luminance behind a dim-recess modal — clearly
            // recessed for focus, still readable for context.
            modal_dim_factor: 0.5,
            primary: Color::Rgb(142, 161, 145),
            warning: Color::Rgb(181, 149, 93),
            success: Color::Rgb(117, 148, 117),
            info: Color::Rgb(128, 153, 156),
            // Diff banding. Lifted from the ad-hoc literals that used to live
            // inline in `draw_diff_content`; kept here as the single source so
            // every block-level surface can share one design contract.
            diff_add_bg: Color::Rgb(18, 31, 22),
            diff_del_bg: Color::Rgb(32, 20, 20),
            diff_add_hl: Color::Rgb(42, 64, 48),
            diff_del_hl: Color::Rgb(64, 40, 40),
        }
    }
}

/// Semantic accessors (ADR-0001 P4): renderers reference intent
/// (surface / body / raised / ok / err / …) rather than the raw palette field
/// names, so the palette can be retuned in one place. The fields stay `pub`
/// for `Theme::default()` construction; new rendering code should prefer these.
impl Theme {
    /// Canonicalize a persisted scheme id. Unknown and empty ids use Zen.
    pub fn normalize_color_scheme(name: &str) -> &'static str {
        COLOR_SCHEMES
            .iter()
            .find(|scheme| scheme.id.eq_ignore_ascii_case(name.trim()))
            .map(|scheme| scheme.id)
            .unwrap_or("zen")
    }

    pub fn color_scheme_index(name: &str) -> usize {
        let name = Self::normalize_color_scheme(name);
        COLOR_SCHEMES
            .iter()
            .position(|scheme| scheme.id == name)
            .unwrap_or(0)
    }

    pub fn color_scheme_label(name: &str) -> &'static str {
        let name = Self::normalize_color_scheme(name);
        COLOR_SCHEMES
            .iter()
            .find(|scheme| scheme.id == name)
            .map(|scheme| scheme.label)
            .unwrap_or("Zen")
    }

    /// Build a complete renderer theme from a preset id or custom semantics.
    pub fn from_color_scheme(name: &str, custom: &ColorSchemeConfig) -> Self {
        match Self::normalize_color_scheme(name) {
            "midnight" => Self::from_semantic(
                Color::Rgb(6, 10, 18),
                Color::Rgb(14, 20, 32),
                Color::Rgb(218, 225, 240),
                Color::Rgb(112, 124, 148),
                Color::Rgb(91, 156, 255),
                Color::Rgb(87, 190, 141),
                Color::Rgb(226, 174, 91),
                Color::Rgb(226, 105, 117),
            ),
            "nord" => Self::from_semantic(
                Color::Rgb(46, 52, 64),
                Color::Rgb(59, 66, 82),
                Color::Rgb(236, 239, 244),
                Color::Rgb(136, 148, 166),
                Color::Rgb(136, 192, 208),
                Color::Rgb(163, 190, 140),
                Color::Rgb(235, 203, 139),
                Color::Rgb(191, 97, 106),
            ),
            "catppuccin" => Self::from_semantic(
                Color::Rgb(30, 30, 46),
                Color::Rgb(49, 50, 68),
                Color::Rgb(205, 214, 244),
                Color::Rgb(147, 153, 178),
                Color::Rgb(203, 166, 247),
                Color::Rgb(166, 227, 161),
                Color::Rgb(249, 226, 175),
                Color::Rgb(243, 139, 168),
            ),
            "paper" => Self::from_semantic(
                Color::Rgb(247, 246, 242),
                Color::Rgb(255, 255, 252),
                Color::Rgb(43, 45, 48),
                Color::Rgb(106, 110, 114),
                Color::Rgb(55, 100, 165),
                Color::Rgb(56, 126, 79),
                Color::Rgb(157, 105, 31),
                Color::Rgb(177, 63, 58),
            ),
            "custom" => Self::from_custom(custom),
            _ => Self::default(),
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
        Self::from_semantic(
            color(&custom.background, &fallback.background),
            color(&custom.surface, &fallback.surface),
            color(&custom.text, &fallback.text),
            color(&custom.muted, &fallback.muted),
            color(&custom.accent, &fallback.accent),
            color(&custom.success, &fallback.success),
            color(&custom.warning, &fallback.warning),
            color(&custom.error, &fallback.error),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn from_semantic(
        background: Color,
        surface: Color,
        text: Color,
        muted: Color,
        accent: Color,
        success: Color,
        warning: Color,
        error: Color,
    ) -> Self {
        let light = luminance(background) > 150.0;
        let code_bg = mix(background, text, if light { 0.06 } else { 0.05 });
        let user_bg = mix(surface, accent, if light { 0.08 } else { 0.10 });
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
            input_bg: mix(surface, text, if light { 0.03 } else { 0.04 }),
            user_panel_bg: user_bg,
            user_panel_bg_queued: mix(background, user_bg, 0.45),
            element_bg: mix(surface, text, if light { 0.035 } else { 0.05 }),
            menu_bg: mix(background, surface, 0.72),
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
    /// Live input-box surface.
    pub fn input_surface(&self) -> Color {
        self.input_bg
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
        for (index, scheme) in COLOR_SCHEMES.iter().enumerate() {
            assert_eq!(Theme::color_scheme_index(scheme.id), index);
        }
    }
}
