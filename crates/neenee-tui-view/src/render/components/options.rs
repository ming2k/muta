//! Option rows for question and choice surfaces.

use neenee_tui::{Line, Modifier, Span, Style};
use unicode_width::UnicodeWidthStr;

use super::super::Theme;
use super::super::text_layout::wrap_text;

pub(in crate::render) struct QuestionOptionRow<'a> {
    pub label: &'a str,
    pub description: Option<&'a str>,
    pub selected: bool,
    pub highlighted: bool,
    pub multi_select: bool,
}

impl<'a> QuestionOptionRow<'a> {
    pub(in crate::render) fn push_lines(
        self,
        lines: &mut Vec<Line<'static>>,
        body_width: usize,
        theme: &Theme,
    ) {
        let (marker, marker_style) = if self.multi_select {
            let marker = if self.selected { "[x]" } else { "[ ]" };
            let style = if self.selected {
                Style::default().fg(theme.ok()).add_modifier(Modifier::BOLD)
            } else if self.highlighted {
                Style::default()
                    .fg(theme.brand())
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.muted())
            };
            (marker, style)
        } else {
            ("", Style::default().fg(theme.muted()))
        };

        let text_style = if self.highlighted {
            Style::default()
                .fg(theme.brand())
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.fg())
        };

        let first_prefix = format!("  {} ", marker);
        let continuation_prefix = "     ";
        push_wrapped_styled_with_prefix_style(
            lines,
            &first_prefix,
            continuation_prefix,
            self.label,
            marker_style,
            text_style,
            body_width,
        );

        if let Some(desc) = self.description {
            let desc_style = if self.highlighted {
                Style::default().fg(theme.brand())
            } else {
                Style::default().fg(theme.dim())
            };
            push_wrapped_styled(lines, "     ", "     ", desc, desc_style, body_width);
        }
    }
}

fn push_wrapped_styled_with_prefix_style(
    lines: &mut Vec<Line<'static>>,
    first_prefix: &str,
    continuation_prefix: &str,
    text: &str,
    first_prefix_style: Style,
    text_style: Style,
    body_width: usize,
) {
    let first_width = first_prefix.width();
    let continuation_width = continuation_prefix.width();
    let wrap_width = body_width
        .saturating_sub(first_width.max(continuation_width))
        .max(1);
    let wrapped = wrap_text(text, wrap_width);
    if wrapped.is_empty() {
        lines.push(Line::from(vec![Span::styled(
            first_prefix.to_string(),
            first_prefix_style,
        )]));
        return;
    }

    for (idx, wrapped_line) in wrapped.into_iter().enumerate() {
        if idx == 0 {
            lines.push(Line::from(vec![
                Span::styled(first_prefix.to_string(), first_prefix_style),
                Span::styled(wrapped_line.text, text_style),
            ]));
        } else {
            lines.push(Line::from(vec![
                Span::styled(continuation_prefix.to_string(), Style::default()),
                Span::styled(wrapped_line.text, text_style),
            ]));
        }
    }
}

fn push_wrapped_styled(
    lines: &mut Vec<Line<'static>>,
    first_prefix: &str,
    continuation_prefix: &str,
    text: &str,
    style: Style,
    body_width: usize,
) {
    let first_width = first_prefix.width();
    let continuation_width = continuation_prefix.width();
    let wrap_width = body_width
        .saturating_sub(first_width.max(continuation_width))
        .max(1);
    let wrapped = wrap_text(text, wrap_width);
    if wrapped.is_empty() {
        return;
    }

    for (idx, wrapped_line) in wrapped.into_iter().enumerate() {
        let prefix = if idx == 0 {
            first_prefix
        } else {
            continuation_prefix
        };
        lines.push(Line::from(vec![
            Span::styled(prefix.to_string(), Style::default()),
            Span::styled(wrapped_line.text, style),
        ]));
    }
}
