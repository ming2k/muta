//! Width-aware modal footer hints.

use neenee_tui::{Frame, Line, Paragraph, Rect, Span, Style};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::super::Theme;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::render) enum FooterPriority {
    Always,
    Primary,
    Navigation,
    Secondary,
}

#[derive(Clone, Copy)]
pub(in crate::render) struct FooterHint {
    pub key: &'static str,
    pub label: &'static str,
    pub priority: FooterPriority,
}

impl FooterHint {
    pub(in crate::render) const fn always(key: &'static str, label: &'static str) -> Self {
        Self {
            key,
            label,
            priority: FooterPriority::Always,
        }
    }

    pub(in crate::render) const fn primary(key: &'static str, label: &'static str) -> Self {
        Self {
            key,
            label,
            priority: FooterPriority::Primary,
        }
    }

    pub(in crate::render) const fn navigation(key: &'static str, label: &'static str) -> Self {
        Self {
            key,
            label,
            priority: FooterPriority::Navigation,
        }
    }

    pub(in crate::render) const fn secondary(key: &'static str, label: &'static str) -> Self {
        Self {
            key,
            label,
            priority: FooterPriority::Secondary,
        }
    }
}

#[derive(Clone, Copy)]
enum FooterLabelMode {
    Full,
    Compact,
}

/// Render the one-line modal command strip with width-aware degradation.
pub(in crate::render) fn render_modal_footer(
    frame: &mut Frame,
    rect: Rect,
    hints: &[FooterHint],
    theme: &Theme,
) {
    frame.render_widget(modal_footer_line(hints, rect.width as usize, theme), rect);
}

pub(in crate::render) fn modal_footer_line(
    hints: &[FooterHint],
    width: usize,
    theme: &Theme,
) -> Paragraph<'static> {
    let text = modal_footer_text(hints, width);
    Paragraph::new(Line::from(Span::styled(
        text,
        Style::default().fg(theme.muted()),
    )))
}

pub(in crate::render) fn modal_footer_text(hints: &[FooterHint], width: usize) -> String {
    if width == 0 || hints.is_empty() {
        return String::new();
    }

    let candidates = [
        (FooterLabelMode::Full, None),
        (FooterLabelMode::Compact, None),
        (FooterLabelMode::Compact, Some(FooterPriority::Navigation)),
        (FooterLabelMode::Full, Some(FooterPriority::Primary)),
        (FooterLabelMode::Compact, Some(FooterPriority::Primary)),
        (FooterLabelMode::Full, Some(FooterPriority::Always)),
        (FooterLabelMode::Compact, Some(FooterPriority::Always)),
    ];

    for (mode, max_priority) in candidates {
        let text = join_footer_hints(hints, mode, max_priority);
        if !text.is_empty() && text.width() <= width {
            return text;
        }
    }

    truncate_to_width(
        &join_footer_hints(
            hints,
            FooterLabelMode::Compact,
            Some(FooterPriority::Always),
        ),
        width,
    )
}

fn join_footer_hints(
    hints: &[FooterHint],
    mode: FooterLabelMode,
    max_priority: Option<FooterPriority>,
) -> String {
    hints
        .iter()
        .filter(|hint| {
            max_priority
                .map(|max| footer_priority_rank(hint.priority) <= footer_priority_rank(max))
                .unwrap_or(true)
        })
        .map(|hint| match mode {
            FooterLabelMode::Full if !hint.label.is_empty() => {
                format!("{} {}", hint.key, hint.label)
            }
            _ => hint.key.to_string(),
        })
        .collect::<Vec<_>>()
        .join(" · ")
}

fn footer_priority_rank(priority: FooterPriority) -> u8 {
    match priority {
        FooterPriority::Always => 0,
        FooterPriority::Primary => 1,
        FooterPriority::Navigation => 2,
        FooterPriority::Secondary => 3,
    }
}

fn truncate_to_width(s: &str, max: usize) -> String {
    if s.width() <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    if max == 1 {
        return "…".to_string();
    }
    let mut out = String::new();
    let mut width = 0usize;
    for c in s.chars() {
        let cw = UnicodeWidthChar::width(c).unwrap_or(0).max(1);
        if width + cw > max - 1 {
            break;
        }
        out.push(c);
        width += cw;
    }
    out.push('…');
    out
}
