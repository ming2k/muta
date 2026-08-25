//! Sessions picker.

use chrono::{Local, TimeZone};
use mutx_engine::{
    Frame, Style, {Line, Span},
};
use unicode_width::UnicodeWidthStr;

use super::common::{one_line, relative_time_at, truncate_ellipsis};
use crate::components::options::{ChoiceStyle, ChoiceTone, choice_style};
use crate::primitives::{
    FixedModalSpec, FooterHint, FooterHintWithBand, SCROLL_EDGE_MARGIN, breadcrumb_parts,
    draw_scrollbar, keymap_body_lines, keymap_page_footer_hints, keyvocab, modal_area, modal_frame,
    modal_header, modal_header_parts, render_centered_body, render_modal_footer,
    render_modal_footer_with_more, resolve_scroll,
};
use crate::view::Theme;

/// Format an epoch-seconds timestamp as a local absolute date-time
/// (`YYYY-MM-DD HH:MM`). Used by the session-info sub-view, where a precise
/// creation/last-active time is more useful than the picker's compact relative
/// form. Falls back to `--` for an out-of-range timestamp.
fn absolute_time(ts: u64) -> String {
    Local
        .timestamp_opt(ts as i64, 0)
        .single()
        .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|| "--".to_string())
}

/// Draw the sessions picker: each row shows the session overview plus its
/// last-active time. Enter opens the selected session; `i` drills into a detail
/// sub-view (full last prompt, creation time, message count). When
/// `keymap_open` is true the body is replaced by the full keybindings list.
/// `scroll` is read AND written back (clamped to the body height) so the modal
/// is scrollable with `PageUp` / `PageDown` / `Ctrl+↑/↓` and the mouse wheel;
/// `follow` keeps the selection on screen after `↑/↓` navigation (cleared on
/// manual scroll, mirroring the other list modals).
///
/// `startup_picker` is `true` only when the picker opened at startup
/// (`mutx attach` with no id). In that mode Esc/click-outside quits the
/// program (there is no conversation behind the modal yet), so the footer
/// hint reads "quit" instead of "close".
///
/// `session_info_detail` switches the body to the detail sub-view for the
/// session under `session_detail` (requested on demand when the sub-view
/// opens); `session_info_scroll` is that sub-view's own scroll slot.
#[allow(clippy::too_many_arguments)]
pub fn draw_sessions_modal(
    frame: &mut Frame,
    sessions: &[muta_contracts::SessionOverview],
    selected: usize,
    keymap_open: bool,
    scroll: &mut usize,
    follow: bool,
    theme: &Theme,
    startup_picker: bool,
    spinner_phase: usize,
    session_info_detail: bool,
    session_detail: Option<&muta_contracts::SessionDetail>,
    session_info_scroll: &mut usize,
    selection: &crate::model::selection::SelectionState,
    layout_map: &mut crate::model::layout::LayoutMap,
) -> mutx_engine::Rect {
    let area = modal_area(frame, FixedModalSpec::SESSIONS);
    let f = modal_frame(frame, area, theme.panel(), true, true);

    // Destructive delete: custom band 70 so it outlives plain secondaries
    // (it is a one-key destructive action the user must be able to find).
    let close_label = if startup_picker { "quit" } else { "close" };
    let list_footer_hints: [FooterHint; 3] = [
        FooterHint::navigation(keyvocab::ARROWS_UD, "navigate"),
        FooterHint::primary(keyvocab::ENTER, "open"),
        FooterHint::always(keyvocab::ESC, close_label),
    ];
    let list_extra: [FooterHintWithBand; 3] = [
        FooterHint::with_band("N", "new", 40),
        FooterHint::with_band("I", "info", 55),
        FooterHint::with_band("D", "delete", 70),
    ];

    if keymap_open {
        // Breadcrumb: `Sessions` modal › its keybindings sub-page.
        modal_header(
            frame,
            f.header,
            &format!("Sessions{}keybindings", crate::design::JOIN_BREADCRUMB),
            theme,
        );
        let body = keymap_body_lines(&list_footer_hints, &list_extra, theme);
        // Selectable document: the keymap sub-page registers as MODAL_DOC
        // rows so key labels and descriptions are copyable.
        let rows: Vec<crate::components::selectable_body::SelectableRow> = body
            .into_iter()
            .map(crate::components::selectable_body::SelectableRow::from_line)
            .collect();
        crate::components::selectable_body::render_selectable_body(
            frame, f.body, &rows, scroll, None, theme, selection, layout_map,
        );
        if let Some(fo) = f.footer {
            render_modal_footer(frame, fo, &keymap_page_footer_hints(), theme);
        }
        return area;
    }

    // Detail sub-view (`i`): a focused read-out of the selected session. Its
    // own footer (Esc → back to list) and own scroll slot; Esc is handled by
    // the event loop's CloseModal arm (first Esc backs out, second closes).
    // The header is a breadcrumb (`Sessions › Info`) — the modal hierarchy
    // convention: a sub-page keeps the same modal but shows where it sits.
    if session_info_detail {
        let header = breadcrumb_parts("Sessions", "Info");
        modal_header_parts(frame, f.header, &header, theme);
        let detail_footer: [FooterHint; 1] = [FooterHint::always(keyvocab::ESC, "list")];
        let body = match session_detail {
            None => {
                const SPINNER_FRAMES: [&str; 10] =
                    ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
                let spin = SPINNER_FRAMES[spinner_phase % SPINNER_FRAMES.len()];
                vec![
                    Line::from(""),
                    Line::from(vec![
                        Span::styled(format!("{spin} "), Style::default().fg(theme.primary)),
                        Span::styled(
                            "Loading session detail…",
                            Style::default().fg(theme.muted()),
                        ),
                    ]),
                ]
            }
            Some(detail) => detail_body(detail, theme),
        };
        // Selectable document: the detail read-out (session id, title,
        // timestamps, last prompt) is exactly the text a user would want to
        // copy out of this sub-view.
        let rows: Vec<crate::components::selectable_body::SelectableRow> = body
            .into_iter()
            .map(crate::components::selectable_body::SelectableRow::from_line)
            .collect();
        crate::components::selectable_body::render_selectable_body(
            frame,
            f.body,
            &rows,
            session_info_scroll,
            None,
            theme,
            selection,
            layout_map,
        );
        if let Some(fo) = f.footer {
            render_modal_footer(frame, fo, &detail_footer, theme);
        }
        return area;
    }

    modal_header(frame, f.header, "Sessions", theme);

    let body_width = f.body.width as usize;

    if sessions.is_empty() {
        // Empty-state: a spinner + hint. Rendered centered directly (no list body).
        const SPINNER_FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
        let spin = SPINNER_FRAMES[spinner_phase % SPINNER_FRAMES.len()];
        let body = vec![Line::from(vec![
            Span::styled(format!("{spin} "), Style::default().fg(theme.primary)),
            Span::styled(
                "Loading sessions / No previous sessions yet.",
                Style::default().fg(theme.muted()),
            ),
        ])];
        render_centered_body(frame, f.body, body);
        if let Some(fo) = f.footer {
            render_modal_footer_with_more(frame, fo, &list_footer_hints, &list_extra, theme);
        }
        return area;
    }

    // Windowed render: only build the rows that will actually be painted. With
    // hundreds of sessions the old code built a `Line` (several allocations +
    // a `SystemTime::now()` syscall each) for *every* row on every drawn frame,
    // even though only `body.height` (~20–40) rows are visible. Resolving the
    // scroll up front (against the true total length) lets us slice the visible
    // window and build just those rows, while the scrollbar still reflects the
    // full list via the resolved `max_scroll`.
    let visible = f.body.height as usize;
    let follow_idx = if follow { Some(selected) } else { None };
    let (start, max_scroll) = resolve_scroll(
        scroll,
        visible,
        sessions.len(),
        follow_idx,
        SCROLL_EDGE_MARGIN,
    );
    // Hoist the wall-clock read out of the per-row loop: it is identical for
    // every row in a frame, so one `SystemTime::now()` replaces one-per-row
    // (≈600 syscalls/frame on a large project).
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let end = (start + visible).min(sessions.len());
    let mut body: Vec<Line> = Vec::with_capacity(end - start);
    for i in start..end {
        let Some(session) = sessions.get(i) else {
            break;
        };
        let is_selected = i == selected;
        let s: ChoiceStyle = choice_style(ChoiceTone::Filled, is_selected, theme);
        // Show only the last-active time in the row meta (creation time is in
        // the info sub-view). Compact relative form keeps the column narrow.
        let meta = if session.active {
            format!("active {}", relative_time_at(session.updated_at, now))
        } else {
            relative_time_at(session.updated_at, now)
        };
        let meta_w = meta.width();
        // Guarantee a fixed gutter between the two columns by giving the
        // overview a width budget of `body_width - meta_w - gutter`, then
        // truncating it with an ellipsis when it overflows. That way a long
        // overview never crowds the meta column, and the gutter is constant
        // row-to-row instead of whatever slack is left over.
        const COL_GUTTER: usize = 2;
        let col1_budget = body_width.saturating_sub(meta_w + COL_GUTTER);
        let overview = truncate_ellipsis(&one_line(&session.overview), col1_budget);
        let left_w = overview.width();
        let pad = body_width.saturating_sub(left_w + meta_w);
        let spans = vec![
            Span::styled(overview, Style::default().bg(s.bg).fg(s.fg)),
            Span::styled(" ".repeat(pad), Style::default().bg(s.bg)),
            Span::styled(meta, Style::default().bg(s.bg).fg(s.dim)),
        ];
        body.push(Line::from(spans));
    }

    // The window is already the visible slice, so render it at scroll 0 and
    // draw the scrollbar against the true `max_scroll` of the full list.
    let para = mutx_engine::Paragraph::new(body);
    frame.render_widget(para, f.body);
    draw_scrollbar(frame, f.body, start, max_scroll, theme);

    if let Some(fo) = f.footer {
        render_modal_footer_with_more(frame, fo, &list_footer_hints, &list_extra, theme);
    }
    area
}

/// Build the session-info sub-view body: a label/value read-out (id, title,
/// created/last-active timestamps, message count) followed by the full last
/// effective user prompt, wrapped to the modal width.
fn detail_body(detail: &muta_contracts::SessionDetail, theme: &Theme) -> Vec<Line<'static>> {
    let label = Style::default().fg(theme.dim());
    let value = Style::default().fg(theme.fg());
    let kv = |k: &str, v: String| {
        Line::from(vec![
            Span::styled(format!("{k}: "), label),
            Span::styled(v, value),
        ])
    };
    let mut lines: Vec<Line> = Vec::new();
    lines.push(kv("ID", detail.id.clone()));
    if let Some(title) = &detail.title {
        lines.push(kv("Title", title.clone()));
    }
    lines.push(kv("Created", absolute_time(detail.created_at)));
    lines.push(kv(
        "Last active",
        format!(
            "{} ({})",
            absolute_time(detail.updated_at),
            relative_time_at(
                detail.updated_at,
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0)
            )
        ),
    ));
    lines.push(kv("Messages", detail.message_count.to_string()));
    if detail.active {
        lines.push(Line::from(vec![
            Span::styled("State: ", label),
            Span::styled("active (this session)", value),
        ]));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled("Last prompt", label)));
    match &detail.last_prompt {
        Some(prompt) => {
            for raw in prompt.lines() {
                // Flatten any stray control chars so the row never spills.
                let flat: String = one_line(raw);
                lines.push(Line::from(Span::styled(flat, value)));
            }
            if prompt.trim().is_empty() {
                lines.push(Line::from(Span::styled(
                    "(empty)",
                    Style::default().fg(theme.muted()),
                )));
            }
        }
        None => lines.push(Line::from(Span::styled(
            "(no user prompt yet)",
            Style::default().fg(theme.muted()),
        ))),
    }
    lines
}
