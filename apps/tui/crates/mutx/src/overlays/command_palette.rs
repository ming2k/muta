//! Authoritative Unified Command Palette (Ctrl+L).
//!
//! Merges Quick Switcher, Which-Key, Actions menu, surface navigation, settings,
//! and rare administrative commands into one searchable, keyboard-first modal.

use mutx_engine::{
    Color, Frame, Modifier, Rect, Style, {Line, Paragraph, Span},
};
use unicode_width::UnicodeWidthStr;

use crate::components::selectable_body::{SelectableRow, render_selectable_body};
use crate::fuzzy::fuzzy_match;
use crate::keymap::{
    AppContext, Availability, COMMAND_REGISTRY, CommandId, CommandSpec, DangerLevel,
};
use crate::model::layout::LayoutMap;
use crate::model::selection::SelectionState;
use crate::primitives::{
    FixedModalSpec, FooterHint, modal_area, modal_frame, modal_header, render_modal_footer,
};
use crate::view::Theme;

/// One selectable entry in the Command Palette list.
#[derive(Debug, Clone)]
pub(crate) struct PaletteEntry {
    pub spec: &'static CommandSpec,
    pub availability: Availability,
    #[allow(dead_code)]
    pub is_recent: bool,
    pub score: i64,
}

/// Filter and rank commands for display in the Command Palette.
pub(crate) fn filter_palette_commands(
    query: &str,
    recent: &[CommandId],
    ctx: &AppContext,
) -> Vec<PaletteEntry> {
    let clean_query = query.trim();

    let mut entries = Vec::new();

    for spec in COMMAND_REGISTRY {
        let avail = (spec.availability)(ctx);
        let is_recent = recent.contains(&spec.id);

        if clean_query.is_empty() {
            entries.push(PaletteEntry {
                spec,
                availability: avail,
                is_recent,
                score: if is_recent { 1000 } else { 0 },
            });
        } else {
            let match_target = format!(
                "{} {} {} {}",
                spec.label,
                spec.hint,
                spec.slash.unwrap_or(""),
                spec.description
            );
            if let Some(m) = fuzzy_match(clean_query, &match_target) {
                entries.push(PaletteEntry {
                    spec,
                    availability: avail,
                    is_recent,
                    score: m.score,
                });
            }
        }
    }

    entries.sort_by(|a, b| {
        let a_avail = matches!(a.availability, Availability::Available);
        let b_avail = matches!(b.availability, Availability::Available);
        match (a_avail, b_avail) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => b
                .score
                .cmp(&a.score)
                .then_with(|| a.spec.label.cmp(b.spec.label)),
        }
    });

    entries
}

/// Properties for rendering the Command Palette modal.
pub(crate) struct CommandPaletteProps<'a> {
    pub query: &'a str,
    pub entries: &'a [PaletteEntry],
    pub selected_index: usize,
    pub scroll: &'a mut usize,
}

/// Draw the unified Command Palette modal.
pub(crate) fn draw_command_palette(
    frame: &mut Frame,
    props: CommandPaletteProps<'_>,
    theme: &Theme,
    selection: &SelectionState,
    layout_map: &mut LayoutMap,
) -> Rect {
    let CommandPaletteProps {
        query,
        entries,
        selected_index,
        scroll,
    } = props;
    let outer_rect = modal_area(frame, FixedModalSpec::PROVIDER);
    let f = modal_frame(frame, outer_rect, theme.panel(), true, true);

    let title = if entries.is_empty() {
        "Commands".to_string()
    } else {
        format!("Commands ({})", entries.len())
    };
    modal_header(frame, f.header, &title, theme);

    // Search query box line at the top of body
    let query_line_rect = Rect {
        x: f.body.x,
        y: f.body.y,
        width: f.body.width,
        height: 1,
    };

    let query_spans = vec![
        Span::styled(
            "> ",
            Style::default()
                .fg(theme.brand())
                .add_modifier(Modifier::BOLD),
        ),
        if query.is_empty() {
            Span::styled(
                "Type a command, slash trigger, or surface name...",
                Style::default().fg(theme.muted()),
            )
        } else {
            Span::styled(
                query,
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            )
        },
    ];
    frame.render_widget(Paragraph::new(Line::from(query_spans)), query_line_rect);

    // Separator line
    let sep_rect = Rect {
        x: f.body.x,
        y: f.body.y + 1,
        width: f.body.width,
        height: 1,
    };
    let sep_str = "─".repeat(sep_rect.width as usize);
    frame.render_widget(
        Paragraph::new(Line::from(vec![Span::styled(
            sep_str,
            Style::default().fg(theme.muted()),
        )])),
        sep_rect,
    );

    let list_rect = Rect {
        x: f.body.x,
        y: f.body.y + 2,
        width: f.body.width,
        height: f.body.height.saturating_sub(2),
    };

    let mut rows: Vec<SelectableRow> = Vec::new();

    if entries.is_empty() {
        rows.push(SelectableRow::from_line(Line::from(vec![Span::styled(
            "  No matching commands found.",
            Style::default().fg(theme.muted()),
        )])));
    } else {
        let body_w = list_rect.width as usize;

        for (i, entry) in entries.iter().enumerate() {
            let is_sel = i == selected_index;
            let avail = matches!(entry.availability, Availability::Available);

            let gutter = if is_sel { "▶ " } else { "  " };
            let gutter_style = if is_sel {
                Style::default()
                    .fg(theme.brand())
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.muted())
            };

            let label_style = if !avail {
                Style::default().fg(theme.muted())
            } else if is_sel {
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.fg())
            };

            let mut left_spans = vec![
                Span::styled(gutter, gutter_style),
                Span::styled(entry.spec.label, label_style),
            ];

            if entry.spec.danger == DangerLevel::Dangerous {
                left_spans.push(Span::raw(" "));
                left_spans.push(Span::styled(
                    "[DANGER]",
                    Style::default()
                        .fg(theme.err())
                        .add_modifier(Modifier::BOLD),
                ));
            } else if entry.spec.danger == DangerLevel::Cautious {
                left_spans.push(Span::raw(" "));
                left_spans.push(Span::styled("[CAUTION]", Style::default().fg(theme.warn())));
            }

            let right_text = match entry.availability {
                Availability::Available => entry.spec.hint,
                Availability::Unavailable(reason) => reason,
            };

            let right_style = if !avail {
                Style::default().fg(theme.muted())
            } else if is_sel {
                Style::default().fg(theme.brand())
            } else {
                Style::default().fg(theme.muted())
            };

            let left_w: usize = left_spans.iter().map(|s| s.content.width()).sum();
            let right_w = right_text.width();

            let row_line = if body_w >= left_w + right_w + 3 {
                let gap = body_w - left_w - right_w - 2;
                let mut spans = left_spans;
                spans.push(Span::raw(" ".repeat(gap)));
                spans.push(Span::styled(right_text, right_style));
                Line::from(spans)
            } else {
                Line::from(left_spans)
            };

            rows.push(SelectableRow::from_line(row_line));
        }
    }

    render_selectable_body(
        frame,
        list_rect,
        &rows,
        scroll,
        Some(selected_index),
        theme,
        selection,
        layout_map,
    );

    if let Some(fo) = f.footer {
        let footer_hints = [
            FooterHint::navigation("↑↓", "move"),
            FooterHint::primary("Enter", "execute"),
            FooterHint::always("Esc", "close"),
        ];
        render_modal_footer(frame, fo, &footer_hints, theme);
    }

    outer_rect
}
