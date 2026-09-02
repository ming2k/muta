//! Todos modal: task list overview with status glyphs, progress counter, and scrollable body.

use mutx_engine::{
    Frame, Paragraph, {Line, Span}, {Modifier, Style},
};

use super::common::todo_status_glyph_color;
use crate::components::selectable_body::{
    RowSegment, SelectableRow, render_selectable_body, selectable_body_desired_rows,
};
use crate::design::MODAL_RUNNER_TITLE_META_GAP;
use crate::primitives::{
    ContentModalSpec, FooterHint, content_modal_area, keyvocab, modal_frame, render_modal_footer,
};
use crate::view::Theme;

/// Inputs for [`draw_todos_modal`].
pub struct TodosModalView<'a> {
    /// Live unified task list, if any.
    pub todos: Option<&'a muta_contracts::TodoList>,
}

/// The Todos modal: a scrollable overview of the task list.
pub fn draw_todos_modal(
    frame: &mut Frame,
    view: TodosModalView<'_>,
    scroll: &mut usize,
    theme: &Theme,
    selection: &crate::model::selection::SelectionState,
    layout_map: &mut crate::model::layout::LayoutMap,
) -> mutx_engine::Rect {
    let TodosModalView { todos } = view;

    let geometry = ContentModalSpec::TODOS;
    let muted = theme.muted();

    let mut rows: Vec<SelectableRow> = Vec::new();

    if let Some(list) = todos.filter(|l| !l.items.is_empty()) {
        for item in &list.items {
            let glyph_color = todo_status_glyph_color(item.status, theme, muted);
            let glyph = item.status.glyph();
            rows.push(
                SelectableRow::styled(&item.content, Style::default().fg(theme.fg()))
                    .with_prefix(RowSegment::styled(
                        format!("{glyph} "),
                        Style::default().fg(glyph_color),
                    ))
                    .with_hang_prefix(RowSegment::styled("  ", Style::default())),
            );
        }
    } else {
        rows.push(SelectableRow::styled(
            "No todos.",
            Style::default().fg(muted),
        ));
    }

    let desired = selectable_body_desired_rows(frame, geometry, &rows);
    let area = content_modal_area(frame, geometry, desired);
    let f = modal_frame(frame, area, theme.panel(), true, true);

    // Header: "Todos" title + trailing done/total counter
    if let Some(h) = f.header {
        let mut header_spans: Vec<Span<'static>> = vec![Span::styled(
            "Todos",
            Style::default()
                .fg(theme.brand())
                .add_modifier(Modifier::BOLD),
        )];
        if let Some(list) = todos.filter(|l| !l.items.is_empty()) {
            use muta_contracts::TodoStatus;
            let done = list.count(TodoStatus::Completed);
            let total = list.items.len();
            header_spans.push(Span::styled(
                format!("{}{done}/{total}", " ".repeat(MODAL_RUNNER_TITLE_META_GAP)),
                Style::default().fg(muted),
            ));
        }
        frame.render_widget(Paragraph::new(Line::from(header_spans)), h);
    }

    render_selectable_body(
        frame, f.body, &rows, scroll, None, theme, selection, layout_map,
    );

    if let Some(footer) = f.footer {
        render_modal_footer(
            frame,
            footer,
            &[
                FooterHint::navigation(keyvocab::ARROWS_UD, "scroll"),
                FooterHint::key_always(crate::keymap::Key::ESC, "close"),
            ],
            theme,
        );
    }
    area
}
