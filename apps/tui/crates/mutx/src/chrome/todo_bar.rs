//! The persistent one-row todo bar summarizing live agent task list.

use mutx_engine::{Frame, Line, Modifier, Paragraph, Rect, Span, Style};
use unicode_width::UnicodeWidthStr;

use crate::components::keycap::keycap_span;
use crate::design::BAR_LEGEND_GAP_MIN;
use crate::keymap::Key;
use crate::view::Theme;

/// How much of the todo bar's `Ctrl+T expand` legend survives under width pressure.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum TodoLegendDensity {
    /// Key + label: `Ctrl+T expand`.
    Full,
    /// Key only: `Ctrl+T`.
    Compact,
}

/// Draw the single-row persistent todo bar floating above the input.
pub fn draw_todo_bar(
    frame: &mut Frame,
    rect: Rect,
    todos: &muta_contracts::TodoList,
    theme: &Theme,
) -> Rect {
    use muta_contracts::{TodoItem, TodoStatus};

    let dim = Style::default().fg(theme.muted());
    let fg = Style::default().fg(theme.fg());
    let bold = Style::default().fg(theme.fg()).add_modifier(Modifier::BOLD);
    let tag_style = Style::default()
        .fg(theme.brand())
        .add_modifier(Modifier::BOLD);
    let full_w = rect.width as usize;

    let done = todos.count(TodoStatus::Completed);
    let total = todos.items.len();
    let progress = format!("{done}/{total}");

    let current: Option<&TodoItem> = todos
        .items
        .iter()
        .find(|i| i.status == TodoStatus::InProgress)
        .or_else(|| todos.items.iter().find(|i| i.status == TodoStatus::Pending));

    let left: Vec<Span<'static>> = vec![
        Span::styled("TODOS", tag_style),
        Span::styled(" ", dim),
        Span::styled(progress, bold),
    ];
    let left_w: usize = left.iter().map(|s| s.content.width()).sum();

    let mk_legend = |density: TodoLegendDensity| -> Vec<Span<'static>> {
        let mut spans: Vec<Span<'static>> = Vec::new();
        spans.push(keycap_span(theme, Key::CTRL_T.display()));
        if matches!(density, TodoLegendDensity::Full) {
            spans.push(Span::styled(" expand", dim));
        }
        spans
    };
    let legend_width =
        |spans: &Vec<Span<'static>>| -> usize { spans.iter().map(|s| s.content.width()).sum() };

    let content_sep = 2;
    let gap_for = |legend_w: usize| if legend_w > 0 { BAR_LEGEND_GAP_MIN } else { 0 };
    const MIN_PREVIEW_WIDTH: usize = 4;
    let preview_budget = |legend_w: usize| {
        let sep = if current.is_some() { content_sep } else { 0 };
        full_w.saturating_sub(left_w + sep + legend_w + gap_for(legend_w))
    };

    let mut legend = mk_legend(TodoLegendDensity::Full);
    if current.is_some() && preview_budget(legend_width(&legend)) < MIN_PREVIEW_WIDTH {
        legend = mk_legend(TodoLegendDensity::Compact);
    }
    if current.is_some() && preview_budget(legend_width(&legend)) < MIN_PREVIEW_WIDTH {
        legend.clear();
    }
    let legend_w = legend_width(&legend);
    let gap = gap_for(legend_w);

    let mut row: Vec<Span<'static>> = Vec::with_capacity(left.len() + 4 + legend.len());
    row.extend(left);

    if let Some(item) = current {
        let budget = preview_budget(legend_w);
        let one_line = crate::overlays::common::one_line(item.content.trim());
        let preview = if one_line.width() > budget {
            crate::overlays::common::truncate_ellipsis(&one_line, budget)
        } else {
            one_line
        };
        let preview_w = preview.width();
        row.push(Span::styled("  ", dim));
        row.push(Span::styled(preview, fg));
        let pad = full_w
            .saturating_sub(left_w + content_sep + preview_w + legend_w)
            .max(gap);
        row.push(Span::styled(" ".repeat(pad), dim));
    } else {
        let pad = full_w.saturating_sub(left_w + legend_w).max(gap);
        row.push(Span::styled(" ".repeat(pad), dim));
    }

    row.extend(legend);

    frame.render_widget(Paragraph::new(Line::from(row)), rect);
    rect
}
