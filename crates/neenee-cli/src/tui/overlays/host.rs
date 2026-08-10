//! Daemon control panel (`/host`, ADR-0096): a live view over every session
//! the unified daemon hosts, with per-row status and a preview of the
//! selected row. Enter switches to a hosted session; mirrored rows (another
//! TUI's session, ADR-0095) are view-only.

use neenee_core::{MonitoredSession, SessionHosting, SessionStatus};
use neenee_tui_engine::{
    Frame, Style, {Line, Span},
};

use crate::tui::primitives::{
    FixedModalSpec, FooterHint, SCROLL_EDGE_MARGIN, keymap_body_lines, keymap_page_footer_hints,
    keyvocab, modal_area, modal_frame, modal_header, render_body, render_modal_footer,
    resolve_scroll,
};
use crate::tui::view::Theme;

/// Draw the control panel. `rows` is the live monitor snapshot the TUI
/// maintains (already sorted newest-first); `current_session_id` highlights
/// the session this TUI is attached to. `scroll`/`follow` mirror the other
/// list modals (windowed render + selection-follow).
#[allow(clippy::too_many_arguments)]
pub fn draw_host_modal(
    frame: &mut Frame,
    rows: &[MonitoredSession],
    selected: usize,
    keymap_open: bool,
    scroll: &mut usize,
    follow: bool,
    theme: &Theme,
    spinner_phase: usize,
    current_session_id: &str,
) -> neenee_tui_engine::Rect {
    let area = modal_area(frame, FixedModalSpec::SESSIONS);
    let f = modal_frame(frame, area, theme.panel(), true, true);

    let footer: [FooterHint; 3] = [
        FooterHint::navigation(keyvocab::ARROWS_UD, "navigate"),
        FooterHint::primary(keyvocab::ENTER, "switch"),
        FooterHint::always(keyvocab::ESC, "close"),
    ];

    if keymap_open {
        modal_header(
            frame,
            f.header,
            &format!("Daemon{}keybindings", crate::tui::design::JOIN_BREADCRUMB),
            theme,
        );
        let body = keymap_body_lines(&footer, &[], theme);
        render_body(
            frame,
            f.body,
            body,
            scroll,
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

    modal_header(frame, f.header, "Daemon", theme);

    if rows.is_empty() {
        const SPINNER_FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
        let spin = SPINNER_FRAMES[spinner_phase % SPINNER_FRAMES.len()];
        let body = vec![
            Line::from(""),
            Line::from(vec![
                Span::styled(format!("{spin} "), Style::default().fg(theme.primary)),
                Span::styled(
                    "No sessions on the daemon yet.",
                    Style::default().fg(theme.muted()),
                ),
            ]),
        ];
        render_body(
            frame,
            f.body,
            body,
            scroll,
            None,
            SCROLL_EDGE_MARGIN,
            false,
            theme,
        );
        if let Some(fo) = f.footer {
            render_modal_footer(frame, fo, &footer, theme);
        }
        return area;
    }

    let visible = f.body.height as usize;
    let follow_idx = if follow { Some(selected) } else { None };
    let (start, max_scroll) =
        resolve_scroll(scroll, visible, rows.len(), follow_idx, SCROLL_EDGE_MARGIN);
    let end = (start + visible).min(rows.len());

    let mut body: Vec<Line> = Vec::with_capacity(end - start + 2);
    for row in &rows[start..end] {
        body.push(host_row(row, row.id == current_session_id, theme));
    }
    // Preview of the selected row: its live detail line (activity / note /
    // context), so the panel doubles as a session preview without a second
    // request.
    if let Some(sel) = rows.get(selected.min(rows.len().saturating_sub(1))) {
        body.push(Line::from(""));
        body.push(preview_line(sel, theme));
    }

    render_body(
        frame,
        f.body,
        body,
        scroll,
        Some(max_scroll),
        SCROLL_EDGE_MARGIN,
        false,
        theme,
    );
    if let Some(fo) = f.footer {
        render_modal_footer(frame, fo, &footer, theme);
    }
    area
}

fn host_row(row: &MonitoredSession, is_current: bool, theme: &Theme) -> Line<'static> {
    let id: String = row.id.chars().take(8).collect();
    let marker = if is_current { "▶ " } else { "  " };
    let hosting = match row.hosting {
        SessionHosting::Hosted => "",
        SessionHosting::Mirrored => " ⇢",
    };
    let status_style = match row.status {
        SessionStatus::Running => Style::default().fg(theme.primary),
        SessionStatus::NeedsApproval | SessionStatus::NeedsInput => {
            Style::default().fg(theme.warn())
        }
        SessionStatus::Failed => Style::default().fg(theme.err()),
        SessionStatus::Idle | SessionStatus::Interrupted => Style::default().fg(theme.muted()),
    };
    Line::from(vec![
        Span::styled(marker.to_string(), Style::default().fg(theme.primary)),
        Span::styled(format!("{id:<9}"), Style::default().fg(theme.fg())),
        Span::styled(format!("{:<14}", row.status.as_str()), status_style),
        Span::styled(
            format!("{hosting} r{} {}tok", row.round, row.output_tokens),
            Style::default().fg(theme.muted()),
        ),
    ])
}

fn preview_line(row: &MonitoredSession, theme: &Theme) -> Line<'static> {
    let mut parts: Vec<String> = Vec::new();
    if let Some(tool) = &row.current_tool {
        parts.push(format!("tool {tool}"));
    }
    if let Some(note) = &row.note {
        parts.push(note.clone());
    } else if let Some(activity) = &row.activity {
        parts.push(activity.clone());
    }
    if let Some(ctx) = row.context_tokens {
        parts.push(format!("ctx {ctx}"));
    }
    if parts.is_empty() && !row.overview.is_empty() {
        parts.push(row.overview.clone());
    }
    let text = if parts.is_empty() {
        "—".to_string()
    } else {
        parts.join(" · ")
    };
    Line::from(vec![
        Span::styled("  ⤷ ", Style::default().fg(theme.primary)),
        Span::styled(text, Style::default().fg(theme.muted())),
    ])
}
