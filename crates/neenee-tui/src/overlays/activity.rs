//! Activity modal: section-specific overview of the current round, pursuit, or todos.
//!
//! Each section is opened independently by clicking the corresponding segment
//! on the activity bar — there is no tab strip or Left/Right cycling.

use neenee_tui_engine::{
    Frame, Paragraph, {Line, Span}, {Modifier, Style},
};

use super::common::todo_status_glyph_color;
use crate::components::selectable_body::{RowSegment, SelectableRow, render_selectable_body};
use crate::design::{MODAL_BODY_LEADING_INDENT, MODAL_TITLE_META_GAP};
use crate::primitives::{
    ContentModalSpec, FooterHint, content_modal_area, keyvocab, modal_chrome_rows, modal_frame,
    render_modal_footer,
};
use crate::view::Theme;

/// Inputs for [`draw_activity_modal`]. Carries everything the old always-pinned
/// pursuit bar, plan panel, and activity bar used to show, gathered into one
/// overlay so the footer is a single line. Fields are `Option`al so the modal
/// simply omits a section when there is nothing to report.
pub struct ActivityModalView<'a> {
    /// Which section to show (Activity or Todos). Each section is opened
    /// independently by clicking the corresponding segment on the activity bar.
    pub active_tab: crate::modal::ActivityTab,
    /// Live unified task list, if any. Shown as a header (done/total) plus
    /// one row per item with a status glyph.
    pub todos: Option<&'a neenee_contracts::TodoList>,
    /// The current round's user prompt, if any. Shown in the Activity tab.
    pub user_prompt: Option<&'a str>,
    /// Harness round counter (`round N`).
    pub round_count: u64,
    /// Current tool turn within the round (1-indexed; `0` before the first
    /// model request).
    pub current_turn: u64,
    /// Display id of the currently active model.
    pub current_model: &'a str,
    /// Wall-clock instant the current round started, or `None` between rounds.
    pub round_started_at: Option<std::time::Instant>,

    pub activity: &'a str,
    /// Ongoing provider retry state, if any.
    pub provider_retry: Option<&'a crate::app::ProviderRetryState>,
}

/// The Activity modal: a scrollable overview of a single section (Activity or
/// Todos). Sized to its content with min/max viewport limits. The active section
/// is determined by which activity-bar segment the user clicked — there is no tab
/// strip inside the modal.
pub fn draw_activity_modal(
    frame: &mut Frame,
    view: ActivityModalView<'_>,
    scroll: &mut usize,
    theme: &Theme,
    selection: &crate::model::selection::SelectionState,
    layout_map: &mut crate::model::layout::LayoutMap,
) -> neenee_tui_engine::Rect {
    let ActivityModalView {
        active_tab,
        todos,
        user_prompt,
        round_count,
        current_turn,
        current_model,
        round_started_at,
        activity,
        provider_retry,
    } = view;

    let geometry = ContentModalSpec::ACTIVITY;
    let muted = theme.muted();

    // The body is a selectable document (`render_selectable_body`): every
    // visual row registers a MODAL_DOC region so the round overview and the
    // todo list are drag-selectable and copyable like transcript text.
    // Decoration (the leading indent, todo status glyphs) is declared as row
    // prefixes, which paint but stay out of copied text; the component wraps
    // in the application layer, replacing the old pre-wrapped
    // `indented_wrapped_lines` emission.
    let indent = || RowSegment::styled(" ".repeat(MODAL_BODY_LEADING_INDENT), Style::default());
    let heading = |text: &str| {
        SelectableRow::styled(
            text,
            Style::default()
                .fg(theme.brand())
                .add_modifier(Modifier::BOLD),
        )
    };
    let body_row =
        |text: &str, style: Style| SelectableRow::styled(text, style).with_prefix(indent());

    let mut rows: Vec<SelectableRow> = Vec::new();

    match active_tab {
        crate::modal::ActivityTab::Activity => {
            let mut have_section = false;

            // ── Prompt (current round's user message) ──
            if let Some(prompt) = user_prompt.filter(|p| !p.is_empty()) {
                if have_section {
                    rows.push(SelectableRow::empty());
                }
                have_section = true;
                rows.push(heading("Prompt"));
                // The component wraps the whole prompt (explicit `\n` and
                // width-induced continuation alike) with the indent as a row
                // prefix, so every visual row of the block inherits the
                // indent — the geometry-property fix the pre-wrap container
                // primitive was introduced for, now expressed declaratively.
                for line in prompt.split('\n') {
                    rows.push(body_row(line, Style::default().fg(theme.fg())));
                }
            }

            // ── Status (always shown) ──
            if have_section {
                rows.push(SelectableRow::empty());
            }
            let idle = activity.is_empty() || activity == "idle";
            rows.push(heading("Status"));

            if round_count > 0 {
                // Build the structured detail as one row so the component
                // wraps it as a unit — a long model name or locale-dependent
                // elapsed string would otherwise overflow the body's right
                // edge. `round › turn` is a container → member breadcrumb (R1
                // would wrongly join two different levels with `·`); model and
                // elapsed are properties of the round (JOIN_MODIFY).
                let mut detail = format!("round {}", round_count);
                if current_turn >= 1 {
                    detail.push_str(crate::design::JOIN_BREADCRUMB);
                    detail.push_str(&format!("turn {}", current_turn));
                }
                if !current_model.is_empty() {
                    detail.push_str(crate::design::JOIN_MODIFY);
                    detail.push_str(current_model);
                }
                if let Some(started) = round_started_at {
                    detail.push_str(crate::design::JOIN_MODIFY);
                    detail.push_str(&crate::chrome::format_elapsed(started.elapsed()));
                }
                rows.push(body_row(&detail, Style::default().fg(muted)));
            }

            let status_style = if idle {
                Style::default().fg(muted)
            } else if provider_retry.is_some() {
                Style::default()
                    .fg(theme.warn())
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
                    .fg(theme.brand())
                    .add_modifier(Modifier::ITALIC)
            };
            let status_label = if let Some(retry) = provider_retry {
                format!(
                    "waiting to retry ({})",
                    retry.summary(std::time::Instant::now())
                )
            } else if idle {
                "idle".to_string()
            } else {
                activity.to_string()
            };
            rows.push(body_row(&status_label, status_style));

            if let Some(retry) = provider_retry.filter(|r| !r.failure.is_empty()) {
                rows.push(SelectableRow::empty());
                rows.push(SelectableRow::styled(
                    "Last failure",
                    Style::default()
                        .fg(theme.warn())
                        .add_modifier(Modifier::BOLD),
                ));
                for line in retry.failure.split('\n') {
                    rows.push(body_row(line, Style::default().fg(theme.fg())));
                }
            }
        }
        crate::modal::ActivityTab::Todos => {
            if let Some(list) = todos.filter(|l| !l.items.is_empty()) {
                // Hanging indent via row prefixes: the status glyph + gutter
                // leads the first visual row, continuation rows align under
                // the content column. Both are decoration (excluded from
                // copy); the component owns the wrapping, so a long task
                // description wraps cleanly instead of spilling past the
                // body's right edge.
                let glyph_col = MODAL_BODY_LEADING_INDENT + 1;
                let content_col = glyph_col + 1;
                for item in &list.items {
                    let glyph_color = todo_status_glyph_color(item.status, theme, muted);
                    let glyph = item.status.glyph();
                    rows.push(
                        SelectableRow::styled(&item.content, Style::default().fg(theme.fg()))
                            .with_prefix(RowSegment::styled(
                                format!("{}{} ", " ".repeat(glyph_col), glyph),
                                Style::default().fg(glyph_color),
                            ))
                            .with_hang_prefix(RowSegment::styled(
                                " ".repeat(content_col),
                                Style::default(),
                            )),
                    );
                }
            } else {
                rows.push(SelectableRow::styled(
                    "No todos.",
                    Style::default().fg(muted),
                ));
            }
        }
    }

    let desired = rows.len() as u16 + modal_chrome_rows(geometry.modal_spec());
    let area = content_modal_area(frame, geometry, desired);
    let f = modal_frame(frame, area, theme.panel(), true, true);

    // ── Header: section title, plus a trailing meta counter for Todos ──
    // The Todos `done/total` counter sits beside the title instead of being
    // re-emitted as a second "Todos" body line, so the label shows once.
    if let Some(h) = f.header {
        let mut header_spans: Vec<Span<'static>> = vec![Span::styled(
            active_tab.title(),
            Style::default()
                .fg(theme.brand())
                .add_modifier(Modifier::BOLD),
        )];
        if let crate::modal::ActivityTab::Todos = active_tab
            && let Some(list) = todos.filter(|l| !l.items.is_empty())
        {
            use neenee_contracts::TodoStatus;
            let done = list.count(TodoStatus::Completed);
            let total = list.items.len();
            header_spans.push(Span::styled(
                format!("{}{done}/{total}", " ".repeat(MODAL_TITLE_META_GAP)),
                Style::default().fg(muted),
            ));
        }
        frame.render_widget(Paragraph::new(Line::from(header_spans)), h);
    }

    // Selectable document body: the component wraps each row (declaration
    // moved from pre-wrapped `Line` emission to `SelectableRow` prefixes
    // above), scrolls, highlights the selection, and registers one region
    // per visual row.
    render_selectable_body(
        frame, f.body, &rows, scroll, None, theme, selection, layout_map,
    );

    if let Some(footer) = f.footer {
        render_modal_footer(
            frame,
            footer,
            &[
                FooterHint::navigation(keyvocab::ARROWS_UD, "scroll"),
                FooterHint::always(keyvocab::ESC, "close"),
            ],
            theme,
        );
    }
    area
}
