//! Unified keyboard-key ("keycap") styling.
//!
//! Every surface that shows a keybinding label to the user — the activity-bar
//! interrupt hint, the Help modal rows, the in-modal keymap page, and the
//! footer hint strip — must route its key text through here so there is a
//! single, consistent key affordance across the app instead of each call site
//! hand-rolling its own `fg`/`bold` combination.
//!
//! The canonical treatment is **brand color + bold**: enough to read as "this
//! is a key you can press" without competing with primary content, and it
//! respects the active theme's accent rather than hard-coding the strongest
//! foreground tone.

use mutx_engine::{Modifier, Span, Style};

use super::super::Theme;

/// The single, theme-aware style applied to every keycap label.
pub(crate) fn keycap_style(theme: &Theme) -> Style {
    Style::default()
        .fg(theme.brand())
        .add_modifier(Modifier::BOLD)
}

/// A styled keycap span for `text`.
pub(crate) fn keycap_span<'a>(theme: &Theme, text: &str) -> Span<'a> {
    Span::styled(text.to_string(), keycap_style(theme))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keycap_style_uses_brand_and_bold() {
        let theme = Theme::default();
        let style = keycap_style(&theme);
        assert_eq!(style.fg, theme.brand());
        assert!(style.add.contains(Modifier::BOLD));
    }
}
