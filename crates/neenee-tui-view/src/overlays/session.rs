//! Sessions picker.

use neenee_tui_engine::{
    Frame, Style, {Line, Span},
};
use unicode_width::UnicodeWidthStr;

use super::common::{one_line, relative_time_compact, truncate_ellipsis};
use crate::components::options::{ChoiceStyle, ChoiceTone, choice_style};
use crate::primitives::{
    FixedModalSpec, FooterHint, FooterHintWithBand, SCROLL_EDGE_MARGIN, keymap_body_lines,
    keymap_page_footer_hints, modal_area, modal_frame, modal_header, render_body,
    render_modal_footer, render_modal_footer_with_more,
};
use crate::view::Theme;

/// Draw the sessions picker: each row shows the session overview plus its
/// creation and last-interaction times. Enter opens the selected session.
/// When `keymap_open` is true the body is replaced by the full keybindings list.
pub fn draw_sessions_modal(
    frame: &mut Frame,
    sessions: &[neenee_core::SessionOverview],
    selected: usize,
    keymap_open: bool,
    theme: &Theme,
) -> neenee_tui_engine::Rect {
    let area = modal_area(frame, FixedModalSpec::SESSIONS);
    let f = modal_frame(frame, area, theme.panel(), true, true);

    // Destructive delete: custom band 70 so it outlives plain secondaries
    // (it is a one-key destructive action the user must be able to find).
    let footer_hints: [FooterHint; 3] = [
        FooterHint::navigation("↑↓", "navigate"),
        FooterHint::primary("Enter", "open"),
        FooterHint::always("Esc", "close"),
    ];
    let extra: [FooterHintWithBand; 1] = [FooterHint::with_band("d", "delete", 70)];

    if keymap_open {
        modal_header(frame, f.header, "Sessions · keybindings", theme);
        let body = keymap_body_lines(&footer_hints, &extra, theme);
        render_body(
            frame,
            f.body,
            body,
            &mut 0,
            None,
            SCROLL_EDGE_MARGIN,
            false,
            theme,
        );
        if let Some(fo) = f.footer {
            render_modal_footer(frame, fo, &keymap_page_footer_hints(), theme);
        }
        return area;
    }

    modal_header(frame, f.header, "Sessions", theme);

    let body_width = f.body.width as usize;
    let mut body: Vec<Line> = Vec::new();

    if sessions.is_empty() {
        body.push(Line::from(""));
        body.push(Line::from(Span::styled(
            "No previous sessions yet.",
            Style::default().fg(theme.muted()),
        )));
    }

    for (i, session) in sessions.iter().enumerate() {
        let is_selected = i == selected;
        let s: ChoiceStyle = choice_style(ChoiceTone::Filled, is_selected, theme);
        let badge = if session.active { "● " } else { "  " };
        // Drop the message count (low signal) and use compact relative times
        // (no "ago" suffix) so the meta column stays narrow and predictable.
        let meta = format!(
            "created {} · active {}",
            relative_time_compact(session.created_at),
            relative_time_compact(session.updated_at),
        );
        let meta_w = meta.width();
        // Guarantee a fixed gutter between the two columns by giving the
        // overview a width budget of `body_width - meta_w - gutter`, then
        // truncating it with an ellipsis when it overflows. That way a long
        // overview never crowds the meta column, and the gutter is constant
        // row-to-row instead of whatever slack is left over.
        const COL_GUTTER: usize = 2;
        let badge_w = badge.width();
        let col1_budget = body_width.saturating_sub(meta_w + COL_GUTTER);
        let overview = truncate_ellipsis(
            &one_line(&session.overview),
            col1_budget.saturating_sub(badge_w),
        );
        let left = format!("{}{}", badge, overview);
        let left_w = left.width();
        let pad = body_width.saturating_sub(left_w + meta_w);
        let spans = vec![
            Span::styled(left, Style::default().bg(s.bg).fg(s.fg)),
            Span::styled(" ".repeat(pad), Style::default().bg(s.bg)),
            Span::styled(meta, Style::default().bg(s.bg).fg(s.dim)),
        ];
        body.push(Line::from(spans));
    }

    render_body(
        frame,
        f.body,
        body,
        &mut 0,
        Some(selected),
        SCROLL_EDGE_MARGIN,
        false,
        theme,
    );

    if let Some(fo) = f.footer {
        render_modal_footer_with_more(frame, fo, &footer_hints, &extra, theme);
    }
    area
}
