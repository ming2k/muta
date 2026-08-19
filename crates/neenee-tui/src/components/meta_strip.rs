//! One-line metadata strips shared by transcript chrome.
//!
//! A [`MetaStrip`] is a horizontal rail made of small replaceable [`MetaChip`]s:
//! anchors (`round 61`, `turn 61`), status chips (`⏸ Queued`), and muted
//! details (`GLM-5.2`, `19:46`).  It centralizes the repeated two-tone
//! "anchor · detail · time" treatment used by assistant turn headers and sent
//! user-message headers.

use std::borrow::Cow;

use neenee_tui_engine::{Color, Frame, Line, Modifier, Paragraph, Rect, Span, Style};
use unicode_width::UnicodeWidthStr;

use crate::text_layout::padded_tail;
use crate::view::Theme;

/// Semantic visual tone for a metadata chip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MetaTone {
    /// Info-tone, bold primary anchor (`round N`, `turn N`).
    Accent,
    /// Secondary metadata (`model`, `HH:MM`, fallback labels).
    Muted,
    /// Warning-tone italic status (`⏸ Queued`).
    WarningItalic,
}

impl MetaTone {
    fn style(self, theme: &Theme) -> Style {
        match self {
            Self::Accent => Style::default()
                .fg(theme.info())
                .add_modifier(Modifier::BOLD),
            Self::Muted => Style::default().fg(theme.muted()),
            Self::WarningItalic => Style::default()
                .fg(theme.warn())
                .add_modifier(Modifier::ITALIC),
        }
    }
}

/// A replaceable piece of metadata inside a [`MetaStrip`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct MetaChip<'a> {
    text: Cow<'a, str>,
    tone: MetaTone,
    separated: bool,
}

impl<'a> MetaChip<'a> {
    fn new(text: impl Into<Cow<'a, str>>, tone: MetaTone, separated: bool) -> Self {
        Self {
            text: text.into(),
            tone,
            separated,
        }
    }
}

/// A one-row metadata strip with optional left padding and background tail fill.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MetaStrip<'a> {
    chips: Vec<MetaChip<'a>>,
    separator: Cow<'a, str>,
    left_pad_cols: usize,
    fill_bg: Option<Color>,
}

impl<'a> Default for MetaStrip<'a> {
    fn default() -> Self {
        Self::new()
    }
}

impl<'a> MetaStrip<'a> {
    pub(crate) fn new() -> Self {
        Self {
            chips: Vec::new(),
            // R1: every trailing chip is a state/measure/attribute of the
            // anchor — the one sanctioned use of the middle dot.
            separator: Cow::Borrowed(crate::design::JOIN_MODIFY),
            left_pad_cols: 0,
            fill_bg: None,
        }
    }

    /// Reserve plain left padding before the first chip. When [`Self::fill_tail`]
    /// is set, the padding uses the same background as the tail.
    pub(crate) fn left_pad(mut self, cols: usize) -> Self {
        self.left_pad_cols = cols;
        self
    }

    /// Fill the remaining row with spaces on `bg`, useful for header rows that
    /// should explicitly repaint their whole surface.
    pub(crate) fn fill_tail(mut self, bg: Color) -> Self {
        self.fill_bg = Some(bg);
        self
    }

    /// Override the inter-chip separator (defaults to [`crate::design::JOIN_MODIFY`]).
    pub(crate) fn separator(mut self, sep: impl Into<Cow<'a, str>>) -> Self {
        self.separator = sep.into();
        self
    }

    /// Add an arbitrary chip with no automatic separator.
    pub(crate) fn chip(mut self, text: impl Into<Cow<'a, str>>, tone: MetaTone) -> Self {
        let text = text.into();
        if !text.as_ref().is_empty() {
            self.chips.push(MetaChip::new(text, tone, false));
        }
        self
    }

    /// Add a leading chip, usually an icon/prefix (`◆ `).
    pub(crate) fn lead(self, text: impl Into<Cow<'a, str>>, tone: MetaTone) -> Self {
        self.chip(text, tone)
    }

    /// Add the primary info-tone anchor (`round N`, `turn N`).
    pub(crate) fn anchor(self, text: impl Into<Cow<'a, str>>) -> Self {
        self.chip(text, MetaTone::Accent)
    }

    /// Add a status chip such as `⏸ Queued`.
    pub(crate) fn status(self, text: impl Into<Cow<'a, str>>, tone: MetaTone) -> Self {
        self.chip(text, tone)
    }

    /// Add muted trailing metadata. A ` · ` separator is inserted only when a
    /// previous visible chip already exists, so detail-only strips degrade
    /// cleanly (`Sent`, not ` · Sent`).
    pub(crate) fn detail(mut self, text: impl Into<Cow<'a, str>>) -> Self {
        let text = text.into();
        if !text.as_ref().is_empty() {
            self.chips.push(MetaChip::new(text, MetaTone::Muted, true));
        }
        self
    }

    /// Render the strip into a one-row rect.
    pub(crate) fn render(self, frame: &mut Frame, rect: Rect, theme: &Theme) {
        let line = self.into_line(rect.width as usize, theme);
        frame.render_widget(Paragraph::new(line), rect);
    }

    fn into_line(self, full_width: usize, theme: &Theme) -> Line<'a> {
        let fill_style = self
            .fill_bg
            .map(|bg| Style::default().bg(bg))
            .unwrap_or_default();
        let mut spans = Vec::with_capacity(self.chips.len().saturating_mul(2) + 2);
        let mut used = 0usize;
        let mut rendered_any = false;

        if self.left_pad_cols > 0 {
            spans.push(Span::styled(" ".repeat(self.left_pad_cols), fill_style));
            used += self.left_pad_cols;
        }

        for chip in self.chips {
            if chip.separated && rendered_any {
                used += self.separator.as_ref().width();
                spans.push(Span::styled(
                    self.separator.to_string(),
                    MetaTone::Muted.style(theme),
                ));
            }
            used += chip.text.as_ref().width();
            spans.push(Span::styled(chip.text, chip.tone.style(theme)));
            rendered_any = true;
        }

        if self.fill_bg.is_some() {
            spans.push(Span::styled(padded_tail(full_width, used), fill_style));
        }

        Line::from(spans)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_separator_uses_join_modify() {
        let theme = Theme::default();
        let strip = MetaStrip::new()
            .lead("> ", MetaTone::Accent)
            .anchor("turn 1")
            .detail("detail 1")
            .detail("detail 2");
        let line = strip.into_line(80, &theme);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "> turn 1 · detail 1 · detail 2");
    }

    #[test]
    fn custom_whitespace_separator_joins_with_spaces() {
        let theme = Theme::default();
        let strip = MetaStrip::new()
            .separator("  ")
            .lead("> ", MetaTone::Accent)
            .anchor("turn 13")
            .detail("glm-5.3 xhigh")
            .detail("13:51");
        let line = strip.into_line(80, &theme);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "> turn 13  glm-5.3 xhigh  13:51");
    }
}
