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

use mutx_engine::{Color, Modifier, Span, Style};
use unicode_width::UnicodeWidthStr;

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

/// An atomic keycap + action label pair, strictly adhering to Visual Language R0.
///
/// An affordance joins a keycap with its action label (e.g. `Ctrl+X menu`,
/// `Ctrl+P block`, `Esc back`). The key and label must never be empty.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct KeyAffordance {
    pub key: &'static str,
    pub label: &'static str,
}

impl KeyAffordance {
    /// Construct a new typed KeyAffordance from a canonical [`Key`].
    pub const fn from_key(key: crate::keymap::Key, label: &'static str) -> Self {
        Self::new(key.display(), label)
    }

    /// Construct a new typed KeyAffordance from a non-single-key glyph (e.g. `keyvocab::ARROWS_UD`).
    pub const fn from_glyph(glyph: &'static str, label: &'static str) -> Self {
        Self::new(glyph, label)
    }

    /// Construct a new typed KeyAffordance.
    ///
    /// # Panics
    /// Panics if `key` or `label` is empty — key affordances require a descriptive action label.
    pub const fn new(key: &'static str, label: &'static str) -> Self {
        assert!(!key.is_empty(), "key token must not be empty");
        assert!(
            !label.is_empty(),
            "label must not be empty — key affordances require a descriptive action"
        );
        Self { key, label }
    }

    /// The visual column width of the keycap + space + label unit.
    pub fn width(&self) -> usize {
        self.key.width() + 1 + self.label.width()
    }

    /// Render this affordance as a pair of styled spans: keycap (brand + bold) + space + label (muted).
    pub fn render_spans(&self, theme: &Theme, bg: Color) -> [Span<'static>; 2] {
        let key_style = keycap_style(theme).bg(bg);
        let label_style = Style::default().bg(bg).fg(theme.muted());
        [
            Span::styled(self.key.to_string(), key_style),
            Span::styled(format!(" {}", self.label), label_style),
        ]
    }
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

    #[test]
    fn key_affordance_renders_atomic_unit() {
        let theme = Theme::default();
        let affordance = KeyAffordance::new("Ctrl+X", "menu");
        assert_eq!(affordance.width(), 6 + 1 + 4);

        let [key_span, label_span] = affordance.render_spans(&theme, theme.body());
        assert_eq!(key_span.content, "Ctrl+X");
        assert_eq!(label_span.content, " menu");
    }

    #[test]
    #[should_panic(expected = "label must not be empty")]
    fn key_affordance_disallows_empty_label() {
        let _ = KeyAffordance::new("Ctrl+X", "");
    }
}
