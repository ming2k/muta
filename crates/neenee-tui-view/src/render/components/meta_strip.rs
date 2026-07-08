//! One-line metadata strips shared by transcript chrome.
//!
//! A [`MetaStrip`] is a horizontal rail made of small replaceable [`MetaChip`]s:
//! anchors (`round 61`, `turn 61`), status chips (`⏸ Queued`), and muted
//! details (`GLM-5.2`, `19:46`).  It centralizes the repeated two-tone
//! "anchor · detail · time" treatment used by assistant round headers and sent
//! user-message headers.

use std::borrow::Cow;

use neenee_tui::{Color, Frame, Line, Modifier, Paragraph, Rect, Span, Style};
use unicode_width::UnicodeWidthStr;

use crate::render::Theme;
use crate::render::text_layout::padded_tail;

/// Semantic visual tone for a metadata chip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::render) enum MetaTone {
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
pub(in crate::render) struct MetaStrip<'a> {
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
    pub(in crate::render) fn new() -> Self {
        Self {
            chips: Vec::new(),
            separator: Cow::Borrowed(" · "),
            left_pad_cols: 0,
            fill_bg: None,
        }
    }

    /// Reserve plain left padding before the first chip. When [`Self::fill_tail`]
    /// is set, the padding uses the same background as the tail.
    pub(in crate::render) fn left_pad(mut self, cols: usize) -> Self {
        self.left_pad_cols = cols;
        self
    }

    /// Fill the remaining row with spaces on `bg`, useful for header rows that
    /// should explicitly repaint their whole surface.
    pub(in crate::render) fn fill_tail(mut self, bg: Color) -> Self {
        self.fill_bg = Some(bg);
        self
    }

    /// Add an arbitrary chip with no automatic separator.
    pub(in crate::render) fn chip(mut self, text: impl Into<Cow<'a, str>>, tone: MetaTone) -> Self {
        let text = text.into();
        if !text.as_ref().is_empty() {
            self.chips.push(MetaChip::new(text, tone, false));
        }
        self
    }

    /// Add a leading chip, usually an icon/prefix (`◆ `).
    pub(in crate::render) fn lead(self, text: impl Into<Cow<'a, str>>, tone: MetaTone) -> Self {
        self.chip(text, tone)
    }

    /// Add the primary info-tone anchor (`round N`, `turn N`).
    pub(in crate::render) fn anchor(self, text: impl Into<Cow<'a, str>>) -> Self {
        self.chip(text, MetaTone::Accent)
    }

    /// Add a status chip such as `⏸ Queued`.
    pub(in crate::render) fn status(self, text: impl Into<Cow<'a, str>>, tone: MetaTone) -> Self {
        self.chip(text, tone)
    }

    /// Add muted trailing metadata. A ` · ` separator is inserted only when a
    /// previous visible chip already exists, so detail-only strips degrade
    /// cleanly (`Sent`, not ` · Sent`).
    pub(in crate::render) fn detail(mut self, text: impl Into<Cow<'a, str>>) -> Self {
        let text = text.into();
        if !text.as_ref().is_empty() {
            self.chips.push(MetaChip::new(text, MetaTone::Muted, true));
        }
        self
    }

    /// Render the strip into a one-row rect.
    pub(in crate::render) fn render(self, frame: &mut Frame, rect: Rect, theme: &Theme) {
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
