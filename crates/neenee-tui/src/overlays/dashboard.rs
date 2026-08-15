//! Session dashboard (`/dashboard`, formerly `/host`; ADR-0096, layout per
//! ADR-0097 §3): a first-class, full-screen orchestration console over every
//! session the unified daemon hosts. The surface is split into two zones:
//!
//! - **Console** (upper, flexible): the AI-interaction region. Today it
//!   carries the orchestrator placeholder and the selected session's live
//!   monitor read-out; ADR-0097 grows it into the orchestrator transcript
//!   with an addressing composer (`@n text`, game-chat style).
//! - **Sessions dock** (bottom strip): every session as a compact card —
//!   sequence number, workspace name (disambiguated when two sessions share
//!   a directory basename), uptime since the session opened, and lifecycle
//!   status. Cards tile into as many columns as the width affords and are
//!   ordered by sequence number (creation order), so positions are stable
//!   while statuses flip around them.
//!
//! Data is the live monitor snapshot the TUI maintains client-side (folded
//! from the daemon's `MonitorEvent` stream), so the dashboard refreshes
//! itself without any extra round-trip. The keyboard defaults to the
//! console/input region (`Tab` drops to the dock). Enter on a dock
//! selection opens the read-only session preview modal; `a` attaches to
//! the selected session (detach + re-attach, leaving the departed session
//! running); `i` / `p` / `n` issue control-plane verbs (interrupt / prompt
//! / new session); `Esc` backs out of the preview, then the dashboard.

use neenee_contracts::{MonitoredSession, SessionHosting, SessionStatus};
use neenee_tui_engine::{
    Constraint, Direction, Frame, Layout, Line, Modifier, Rect, Span, Style,
    {Block as RtBlock, Clear, Paragraph},
};
use unicode_width::UnicodeWidthStr;

use crate::primitives::{
    FooterHint, SCROLL_EDGE_MARGIN, keymap_body_lines, keymap_page_footer_hints, keyvocab,
    render_modal_footer_with_more, resolve_scroll, viewport_rect,
};
use crate::view::Theme;

/// Which zone of the dashboard currently owns the keyboard.
#[derive(PartialEq, Eq, Clone, Copy, Debug)]
pub enum DashboardFocus {
    /// The sessions dock (bottom strip): ↑/↓ moves the selection; Enter
    /// opens its preview modal, `a` attaches.
    List,
    /// The console / input region (upper, the default): ↑/↓ scrolls the
    /// interaction surface. The dashboard opens here (ADR-0097 §3) so the
    /// orchestrator composer owns typing without a Tab first.
    Detail,
}

/// The result of laying out one frame: the sub-rects the event loop records
/// for hit-testing and scroll math. Returned by [`draw_dashboard`].
pub struct DashboardRects {
    /// The whole dashboard surface (the viewport).
    pub area: Rect,
    /// The console body (chrome excluded) — the scroll viewport the event
    /// loop sizes PageUp/PageDown against.
    pub list_body: Rect,
    /// The sessions-dock body when visible. Reserved for future hit-testing.
    #[allow(dead_code)]
    pub detail_body: Option<Rect>,
}

/// A session card's dock position: its 1-based sequence number (creation
/// order across the whole snapshot) and its disambiguated workspace name.
struct DockEntry<'a> {
    row: &'a MonitoredSession,
    seq: usize,
    workspace: String,
}

/// The dashboard's sequence axis: indices into `rows` ordered by creation
/// (`created_at` ascending, id tiebreak so the order is total and stable).
/// The renderer and every selection-driven action (attach / interrupt /
/// prompt) must agree on this mapping — a positional selection means the
/// `#seq`-th card, not the `selected`-th snapshot row.
pub fn creation_order(rows: &[MonitoredSession]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..rows.len()).collect();
    order.sort_by(|&a, &b| {
        (rows[a].created_at, &rows[a].id).cmp(&(rows[b].created_at, &rows[b].id))
    });
    order
}

/// Order the snapshot by creation (the sequence axis) and precompute each
/// session's display fields: `#seq` numbers and workspace names that are
/// unique-by-basename when possible, full-path-disambiguated when two
/// sessions share a directory name (ADR-0097 §3).
fn dock_entries(rows: &[MonitoredSession]) -> Vec<DockEntry<'_>> {
    let order = creation_order(rows);

    // Basename collisions → the colliding entries fall back to the full path.
    let base_names: Vec<String> = rows
        .iter()
        .map(|row| workspace_basename(&row.project_root))
        .collect();
    let mut counts = std::collections::HashMap::new();
    for name in &base_names {
        *counts.entry(name.clone()).or_insert(0usize) += 1;
    }

    let mut entries: Vec<Option<DockEntry>> = (0..rows.len()).map(|_| None).collect();
    for (seq, idx) in order.into_iter().enumerate() {
        let row = &rows[idx];
        let workspace = if row.project_root.is_empty() {
            "—".to_string()
        } else if counts.get(&base_names[idx]).copied().unwrap_or(0) > 1 {
            row.project_root.clone()
        } else {
            base_names[idx].clone()
        };
        entries[idx] = Some(DockEntry {
            row,
            seq: seq + 1,
            workspace,
        });
    }
    entries
        .into_iter()
        .enumerate()
        .map(|(idx, e)| e.unwrap_or_else(|| unreachable!("index {idx} filled above")))
        .collect()
}

/// The workspace directory's own name — never a parent path segment (a bare
/// basename like `src` carries no project signal when the workspace *is* a
/// src directory; the full path takes over on collision instead). Roots and
/// otherwise nameless paths stay verbatim.
fn workspace_basename(project_root: &str) -> String {
    let trimmed = project_root.trim_end_matches(['/', '\\']);
    if trimmed.is_empty() {
        // The path was nothing but separators (e.g. "/"): show it as-is.
        return project_root.to_string();
    }
    trimmed
        .rsplit(['/', '\\'])
        .next()
        .filter(|name| !name.is_empty())
        .unwrap_or(trimmed)
        .to_string()
}

/// Draw the full-screen dashboard and return its sub-rects.
///
/// `rows` is the live monitor snapshot (re-sorted internally by creation
/// order for the dock); `selected` indexes the *creation-ordered* dock
/// entries; `current_session_id` marks the session this TUI is attached to.
/// `focus` selects whether ↑/↓/PgUp/PgDn move the dock selection or scroll
/// the console. `prompting` shows the inline new-session prompt line in
/// place of the footer command strip.
#[allow(clippy::too_many_arguments)]
pub fn draw_dashboard(
    frame: &mut Frame,
    rows: &[MonitoredSession],
    selected: usize,
    focus: DashboardFocus,
    keymap_open: bool,
    list_scroll: &mut usize,
    list_follow: bool,
    detail_scroll: &mut usize,
    prompting: bool,
    // `prompt_create_new`: `true` when the open prompt creates a new session
    // (`n`), `false` when it prompts the selected session (`p`). Only
    // meaningful when `prompting`.
    prompt_create_new: bool,
    prompt_text: &str,
    theme: &Theme,
    spinner_phase: usize,
    current_session_id: &str,
) -> DashboardRects {
    // A true full-screen surface: clear the whole frame and paint our own
    // backdrop, then lay out inside the viewport margins.
    frame.render_widget(Clear, frame.area());
    frame.render_widget(
        RtBlock::default().style(Style::default().bg(theme.app_bg)),
        frame.area(),
    );
    let area = viewport_rect(frame);

    // Vertical chrome: header / gap / console (flex) / gap / sessions dock /
    // gap / footer. The console takes everything the dock doesn't need —
    // the dock sizes itself to its content (capped at half the viewport).
    let entries = dock_entries(rows);
    let card_columns = dock_columns(area.width);
    let dock_rows = entries.len().div_ceil(card_columns);
    // Card rows + panel title. One leading card row is always reserved so
    // the empty state ("press n") has a home.
    let dock_height = (dock_rows.max(1) as u16 + 1).min(area.height / 2);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),           // header
            Constraint::Length(1),           // gap
            Constraint::Min(0),              // console
            Constraint::Length(1),           // gap
            Constraint::Length(dock_height), // sessions dock
            Constraint::Length(1),           // gap
            Constraint::Length(1),           // footer
        ])
        .split(area);
    let header = chunks[0];
    let console = chunks[2];
    let dock = chunks[4];
    let footer = chunks[6];

    draw_header(frame, header, rows, theme);

    if keymap_open {
        let body_lines = keymap_body_lines(&footer_hints(focus), &[], theme);
        render_scrollable(frame, console, body_lines, list_scroll, None, theme);
        render_footer(frame, footer, focus, false, false, "", theme, true);
        return DashboardRects {
            area,
            list_body: console,
            detail_body: None,
        };
    }

    let selected = selected.min(entries.len().saturating_sub(1));
    let console_body = draw_console(
        frame,
        console,
        entries.get(selected).map(|e| e.row),
        focus == DashboardFocus::Detail,
        detail_scroll,
        theme,
        spinner_phase,
    );

    let dock_body = draw_dock(
        frame,
        dock,
        &entries,
        card_columns,
        selected,
        focus == DashboardFocus::List,
        list_scroll,
        list_follow,
        theme,
        current_session_id,
    );

    render_footer(
        frame,
        footer,
        focus,
        prompting,
        prompt_create_new,
        prompt_text,
        theme,
        false,
    );

    DashboardRects {
        area,
        list_body: console_body,
        detail_body: Some(dock_body),
    }
}

/// The dashboard's head row: `DASHBOARD` identity and scope on the left, a
/// live session-count summary on the right. Matches the head chrome every
/// other view (session / envoy / btw) carries on its first row.
fn draw_header(frame: &mut Frame, header: Rect, rows: &[MonitoredSession], theme: &Theme) {
    let needing = rows
        .iter()
        .filter(|r| {
            matches!(
                r.status,
                SessionStatus::NeedsApproval | SessionStatus::NeedsInput | SessionStatus::Failed
            )
        })
        .count();
    let running = rows
        .iter()
        .filter(|r| r.status == SessionStatus::Running)
        .count();
    let summary = if rows.is_empty() {
        "no sessions ".to_string()
    } else {
        let mut parts = vec![format!("{} session(s)", rows.len())];
        if running > 0 {
            parts.push(format!("{running} running"));
        }
        if needing > 0 {
            parts.push(format!("{needing} need attention"));
        }
        format!("{} ", parts.join(" · "))
    };

    let title = " DASHBOARD ";
    let context = "all projects";
    let fill = Style::default().bg(theme.body());
    let title_width = title.len();
    let context_width = 1 + context.len(); // leading space separator
    let summary_width = summary.len();
    let gap = (header.width as usize).saturating_sub(title_width + context_width + summary_width);

    let line = Line::from(vec![
        Span::styled(
            title.to_string(),
            fill.fg(theme.fg()).add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!(" {context}"), fill.fg(theme.brand())),
        Span::styled(" ".repeat(gap), fill),
        Span::styled(summary, fill.fg(theme.muted())),
    ]);
    frame.render_widget(Paragraph::new(line), header);
}

/// Render the session list. Returns the body rect used for rows (the scroll
/// viewport — but for card *rows*, not individual sessions: scrolling the
/// dock pages whole card rows at once.
#[allow(clippy::too_many_arguments)]
fn draw_dock(
    frame: &mut Frame,
    area: Rect,
    entries: &[DockEntry<'_>],
    card_columns: usize,
    selected: usize,
    focused: bool,
    scroll: &mut usize,
    follow: bool,
    theme: &Theme,
    current_session_id: &str,
) -> Rect {
    // Panel chrome: a titled band so the dock reads as a pane.
    let (panel, body) = inset_panel(frame, area, "Sessions", focused, theme);
    let _ = panel;

    if entries.is_empty() {
        render_scrollable(
            frame,
            body,
            vec![Line::from(vec![
                Span::styled(
                    "No sessions on the daemon yet.".to_string(),
                    Style::default().fg(theme.muted()),
                ),
                Span::styled(
                    "  Press n to create one.".to_string(),
                    Style::default().fg(theme.dim()),
                ),
            ])],
            scroll,
            None,
            theme,
        );
        return body;
    }

    // Card width is an even share of the body; the last column can be wider
    // (remainder), which only gives its name field more room.
    let total_rows = entries.len().div_ceil(card_columns);
    let visible_rows = body.height as usize;
    let selected_card_row = selected / card_columns;
    let follow_idx = if follow {
        Some(selected_card_row)
    } else {
        None
    };
    let (start_row, _) = resolve_scroll(
        scroll,
        visible_rows,
        total_rows,
        follow_idx,
        SCROLL_EDGE_MARGIN,
    );
    let end_row = (start_row + visible_rows).min(total_rows);

    let col_w = (body.width as usize / card_columns).max(1);
    let now = now_unix_secs();
    for (rel, card_row) in (start_row..end_row).enumerate() {
        for col in 0..card_columns {
            let idx = card_row * card_columns + col;
            let Some(entry) = entries.get(idx) else { break };
            let cell = Rect {
                x: body.x + (col * col_w) as u16,
                y: body.y + rel as u16,
                width: if col + 1 == card_columns {
                    body.width - (col * col_w) as u16
                } else {
                    col_w as u16
                },
                height: 1,
            };
            let is_selected = idx == selected;
            if is_selected {
                frame.render_widget(Clear, cell);
                frame.render_widget(
                    RtBlock::default().style(Style::default().bg(theme.raised())),
                    cell,
                );
            }
            let line = dock_card_line(
                entry,
                cell.width as usize,
                entry.row.id == current_session_id,
                is_selected,
                now,
                theme,
            );
            frame.render_widget(Paragraph::new(line), cell);
        }
    }
    crate::primitives::draw_scrollbar(
        frame,
        body,
        start_row,
        total_rows.saturating_sub(visible_rows),
        theme,
    );
    body
}

/// How many card columns the dock's width affords. One column per
/// [`DOCK_COLUMN_W`] cells, so the strip degrades to a single column on
/// narrow terminals and spreads out on wide ones.
fn dock_columns(body_width: u16) -> usize {
    (body_width as usize / DOCK_COLUMN_W).clamp(1, 4)
}

/// Target width of one session card. Sized so a card always fits the
/// sequence tag, a useful slice of workspace name, uptime, and status.
const DOCK_COLUMN_W: usize = 36;

/// One session card as a single line of spans:
/// `#3  neenee  2h14m  running` — sequence · workspace · uptime · status.
/// The workspace name is the flexible field, truncated to fit; everything
/// else has a fixed reservation.
fn dock_card_line(
    entry: &DockEntry<'_>,
    cell_width: usize,
    is_current: bool,
    is_selected: bool,
    now: u64,
    theme: &Theme,
) -> Line<'static> {
    let row = entry.row;
    let seq = format!("#{}", entry.seq);
    let marker = if is_current { "▶" } else { " " };
    let uptime = format_uptime(now.saturating_sub(row.created_at));
    let status = dock_status_label(row.status);
    // The name is the card's identity: bold it under selection, plain
    // otherwise — the band background carries the rest of the highlight.
    let base_style = Style::default().fg(theme.fg());
    let name_style = if is_selected {
        base_style.add_modifier(Modifier::BOLD)
    } else {
        base_style
    };

    // Fixed reservations: marker+space (2) · seq (5) · uptime (7) ·
    // status (15) · three 2-cell gutters (6). The name takes the rest.
    const FIXED: usize = 2 + 5 + 7 + 15 + 6;
    let name_w = cell_width.saturating_sub(FIXED).max(4);
    let name = truncate_display(&entry.workspace, name_w);

    let spans = vec![
        Span::styled(format!("{marker} "), Style::default().fg(theme.brand())),
        Span::styled(format!("{seq:<5}"), Style::default().fg(theme.brand())),
        Span::styled(format_padded(&name, name_w), name_style),
        Span::styled("  ".to_string(), Style::default()),
        Span::styled(format!("{uptime:>7}"), Style::default().fg(theme.muted())),
        Span::styled("  ".to_string(), Style::default()),
        Span::styled(format!("{status:<15}"), status_style(row.status, theme)),
    ];
    Line::from(spans)
}

/// The dock's coarse status vocabulary: running vs. done, with the blocked
/// states kept distinct since they are the dashboard's whole reason to be.
fn dock_status_label(status: SessionStatus) -> &'static str {
    match status {
        SessionStatus::Running => "running",
        SessionStatus::NeedsApproval => "needs approval",
        SessionStatus::NeedsInput => "needs input",
        SessionStatus::Interrupted => "interrupted",
        SessionStatus::Failed => "failed",
        SessionStatus::Idle => "done",
    }
}

/// Seconds-since-open as a compact uptime: `43s`, `12m`, `2h14m`, `3d5h`.
fn format_uptime(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86_400 {
        format!("{}h{}m", secs / 3600, (secs % 3600) / 60)
    } else {
        format!("{}d{}h", secs / 86_400, (secs % 86_400) / 3600)
    }
}

/// One `SystemTime::now()` per frame for every uptime in the dock (the
/// session picker's precedent: `tui/overlays/session.rs`).
fn now_unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Width-aware truncation with an ellipsis tail (mirrors `truncate_str` in
/// the token report's overlay, kept local so the dock is self-contained).
fn truncate_display(text: &str, max_width: usize) -> String {
    if text.width() <= max_width {
        text.to_string()
    } else if max_width <= 1 {
        "…".to_string()
    } else {
        let mut out: String = text.chars().take(max_width - 1).collect();
        out.push('…');
        out
    }
}

/// Left-align `text` into a fixed display-width field with trailing spaces.
fn format_padded(text: &str, width: usize) -> String {
    let w = text.width();
    if w >= width {
        text.to_string()
    } else {
        let mut out = text.to_string();
        out.push_str(&" ".repeat(width - w));
        out
    }
}

/// The console: the dashboard's upper, flexible AI-interaction region.
/// Until ADR-0097's orchestrator lands it holds the orchestrator placeholder
/// plus the selected session's live monitor read-out (the "monitor" half of
/// the old split). Scrolling this pane is what `Detail` focus drives.
fn draw_console(
    frame: &mut Frame,
    area: Rect,
    row: Option<&MonitoredSession>,
    focused: bool,
    scroll: &mut usize,
    theme: &Theme,
    spinner_phase: usize,
) -> Rect {
    let (_, body) = inset_panel(frame, area, "Console", focused, theme);

    // Orchestrator placeholder (ADR-0097 §3-§4): until the daemon-level
    // orchestrator and its addressing composer land, this surface hosts a
    // hint strip plus the selected session's monitor read-out.
    let mut lines: Vec<Line> = Vec::new();
    const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    let spin = SPINNER[spinner_phase % SPINNER.len()];
    lines.push(Line::from(vec![
        Span::styled(format!("{spin} "), Style::default().fg(theme.brand())),
        Span::styled(
            "Orchestrator console — direct the fleet from here.".to_string(),
            Style::default().fg(theme.muted()),
        ),
    ]));
    lines.push(Line::from(Span::styled(
        "Compose with @n to address a session once the composer lands (ADR-0097); \
         until then p prompts the selected session, n starts a new one."
            .to_string(),
        Style::default().fg(theme.dim()),
    )));
    lines.push(Line::from(""));

    let Some(row) = row else {
        render_scrollable(frame, body, lines, scroll, None, theme);
        return body;
    };

    lines.push(Line::from(Span::styled(
        "selected session".to_string(),
        Style::default().fg(theme.dim()),
    )));
    lines.extend(session_detail_lines(row, body.width as usize, theme));

    render_scrollable(frame, body, lines, scroll, None, theme);
    body
}

// ── shared chrome ─────────────────────────────────────────────────────────

/// Paint a panel band with a small title and return (outer, inner-body).
/// `focused` brightens the title so the active pane reads as such.
fn inset_panel(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    focused: bool,
    theme: &Theme,
) -> (Rect, Rect) {
    frame.render_widget(
        RtBlock::default().style(Style::default().bg(theme.panel())),
        area,
    );
    // Title row at the top of the panel.
    let title_rect = Rect {
        x: area.x + 1,
        y: area.y,
        width: area.width.saturating_sub(1),
        height: 1,
    };
    let title_style = if focused {
        Style::default()
            .fg(theme.brand())
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.muted())
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(title.to_string(), title_style))),
        title_rect,
    );
    // Body: everything below the title, inset one cell on each side.
    let body = Rect {
        x: area.x + 1,
        y: area.y + 1,
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(1),
    };
    (area, body)
}

/// Render a scrollable body (no follow / selection), clamping scroll.
fn render_scrollable(
    frame: &mut Frame,
    body: Rect,
    lines: Vec<Line<'static>>,
    scroll: &mut usize,
    follow: Option<usize>,
    theme: &Theme,
) {
    let visible = body.height as usize;
    let (_, max_scroll) = resolve_scroll(scroll, visible, lines.len(), follow, SCROLL_EDGE_MARGIN);
    let para = Paragraph::new(lines).scroll(*scroll as u16, 0);
    frame.render_widget(para, body);
    crate::primitives::draw_scrollbar(frame, body, *scroll, max_scroll, theme);
}

#[allow(clippy::too_many_arguments)]
fn render_footer(
    frame: &mut Frame,
    rect: Rect,
    focus: DashboardFocus,
    prompting: bool,
    prompt_create_new: bool,
    prompt_text: &str,
    theme: &Theme,
    keymap_page: bool,
) {
    if prompting {
        // Inline prompt: a simple editable line (the composer is borrowed as
        // the input buffer by the event loop). The label reflects whether this
        // creates a session (`n`) or prompts the selected one (`p`).
        let (label, hint) = if prompt_create_new {
            ("New session task: ", "   (Enter create · Esc cancel)")
        } else {
            ("Send task: ", "   (Enter send · Esc cancel)")
        };
        let line = Line::from(vec![
            Span::styled(
                label.to_string(),
                Style::default()
                    .fg(theme.brand())
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(prompt_text.to_string(), Style::default().fg(theme.fg())),
            Span::styled(hint.to_string(), Style::default().fg(theme.dim())),
        ]);
        frame.render_widget(Paragraph::new(line), rect);
        return;
    }
    if keymap_page {
        // The keymap page swaps in its own footer hints.
        crate::primitives::render_modal_footer(frame, rect, &keymap_page_footer_hints(), theme);
        return;
    }
    render_modal_footer_with_more(frame, rect, &footer_hints(focus), &[], theme);
}

fn footer_hints(_focus: DashboardFocus) -> [FooterHint; 6] {
    [
        FooterHint::navigation(keyvocab::ARROWS_UD, "navigate"),
        FooterHint::primary(keyvocab::ENTER, "preview"),
        FooterHint::always(keyvocab::TAB, "switch pane"),
        FooterHint::secondary("a", "attach"),
        FooterHint::secondary("i", "interrupt"),
        FooterHint::secondary("p", "prompt"),
    ]
}

fn status_style(status: SessionStatus, theme: &Theme) -> Style {
    match status {
        SessionStatus::Running => Style::default().fg(theme.brand()),
        SessionStatus::NeedsApproval | SessionStatus::NeedsInput => {
            Style::default().fg(theme.warn())
        }
        SessionStatus::Failed => Style::default().fg(theme.err()),
        SessionStatus::Idle | SessionStatus::Interrupted => Style::default().fg(theme.muted()),
    }
}

fn format_elapsed(ms: u64) -> String {
    let secs = ms / 1000;
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m{}s", secs / 60, secs % 60)
    } else {
        format!("{}h{}m", secs / 3600, (secs % 3600) / 60)
    }
}

/// The full monitor read-out for one session as label/value lines, shared
/// by the console's "selected session" block and the preview modal. `width`
/// is the render width for wrapping the overview.
fn session_detail_lines(row: &MonitoredSession, width: usize, theme: &Theme) -> Vec<Line<'static>> {
    let mut lines: Vec<Line> = Vec::new();
    let mut field = |label: &str, value: String, style: Style| {
        lines.push(Line::from(vec![
            Span::styled(format!("{label:<12}"), Style::default().fg(theme.dim())),
            Span::styled(value, style),
        ]));
    };
    field("session", row.id.clone(), Style::default().fg(theme.fg()));
    if !row.project_root.is_empty() {
        field(
            "workspace",
            row.project_root.clone(),
            Style::default().fg(theme.fg()),
        );
    }
    field(
        "status",
        row.status.as_str().to_string(),
        status_style(row.status, theme),
    );
    field(
        "hosting",
        match row.hosting {
            SessionHosting::Hosted => "hosted".to_string(),
        },
        Style::default().fg(theme.fg()),
    );
    let round = match row.turn {
        Some(t) => format!("round {} · turn {}", row.round, t),
        None => format!("round {}", row.round),
    };
    field("progress", round, Style::default().fg(theme.fg()));
    field(
        "output",
        format!("{} tokens", row.output_tokens),
        Style::default().fg(theme.fg()),
    );
    field(
        "uptime",
        format_uptime(now_unix_secs().saturating_sub(row.created_at)),
        Style::default().fg(theme.fg()),
    );
    field(
        "elapsed",
        format_elapsed(row.elapsed_ms),
        Style::default().fg(theme.fg()),
    );
    if let Some(ctx) = row.context_tokens {
        field(
            "context",
            format!("{ctx} tokens"),
            Style::default().fg(theme.fg()),
        );
    }
    if let Some(tool) = &row.current_tool {
        field("tool", tool.clone(), Style::default().fg(theme.brand()));
    }
    if let Some(activity) = &row.activity {
        field(
            "activity",
            activity.clone(),
            Style::default().fg(theme.fg()),
        );
    }
    if let Some(note) = &row.note {
        field("note", note.clone(), Style::default().fg(theme.warn()));
    }
    field(
        "messages",
        row.message_count.to_string(),
        Style::default().fg(theme.fg()),
    );

    if !row.overview.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "overview".to_string(),
            Style::default().fg(theme.dim()),
        )));
        let wrap_w = width.saturating_sub(1).max(8);
        for chunk in wrap_text(&row.overview, wrap_w) {
            lines.push(Line::from(Span::styled(
                chunk,
                Style::default().fg(theme.muted()),
            )));
        }
    }
    lines
}

/// The session preview modal (ADR-0097 §3): a centered, read-only drill-in
/// on one session, opened by Enter on a dock selection. Selection alone
/// never opens it; Esc closes. Read-only — attaching is `a` on the dock.
///
/// Until the daemon's read-only transcript verb lands (a separate ADR), the
/// body is the full monitor read-out for the session rather than its
/// conversation.
pub fn draw_session_preview(
    frame: &mut Frame,
    row: Option<&MonitoredSession>,
    scroll: &mut usize,
    theme: &Theme,
) {
    let viewport = viewport_rect(frame);
    let area = crate::primitives::centered_rect(62, 72, viewport);
    // Occlude the dashboard beneath and paint the panel.
    frame.render_widget(Clear, area);
    frame.render_widget(
        RtBlock::default().style(Style::default().bg(theme.panel())),
        area,
    );

    // Vertical chrome: title / gap / body / gap / footer-hint.
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(area);
    let title = chunks[0];
    let body = chunks[2];
    let footer = chunks[4];

    let mut title_spans = vec![Span::styled(
        " Session preview".to_string(),
        Style::default()
            .fg(theme.brand())
            .add_modifier(Modifier::BOLD),
    )];
    if let Some(r) = row {
        title_spans.push(Span::styled(
            format!("  —  {}", r.id),
            Style::default().fg(theme.muted()),
        ));
    }
    frame.render_widget(Paragraph::new(Line::from(title_spans)), title);

    let lines = match row {
        Some(r) => session_detail_lines(r, body.width as usize, theme),
        None => vec![Line::from(Span::styled(
            "Session is no longer on the daemon.".to_string(),
            Style::default().fg(theme.muted()),
        ))],
    };
    render_scrollable(frame, body, lines, scroll, None, theme);

    let hint = Line::from(vec![
        Span::styled(" ↑↓".to_string(), Style::default().fg(theme.brand())),
        Span::styled(" scroll   ".to_string(), Style::default().fg(theme.muted())),
        Span::styled("Esc".to_string(), Style::default().fg(theme.brand())),
        Span::styled(" close".to_string(), Style::default().fg(theme.muted())),
    ]);
    frame.render_widget(Paragraph::new(hint), footer);
}

/// Greedy word-wrap into `width` columns (character count; good enough for a
/// preview pane — wide glyphs are not split mid-codepoint).
fn wrap_text(text: &str, width: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        let wlen = word.chars().count();
        let cur = line.chars().count();
        if cur > 0 && cur + 1 + wlen > width {
            out.push(std::mem::take(&mut line));
            line.push_str(word);
        } else {
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(word);
        }
    }
    if !line.is_empty() {
        out.push(line);
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(
        id: &str,
        created_at: u64,
        project_root: &str,
        status: SessionStatus,
    ) -> MonitoredSession {
        MonitoredSession {
            id: id.into(),
            overview: format!("task for {id}"),
            created_at,
            updated_at: created_at,
            message_count: 3,
            hosting: SessionHosting::Hosted,
            status,
            round: 1,
            turn: None,
            output_tokens: 120,
            elapsed_ms: 5_000,
            current_tool: None,
            activity: None,
            context_tokens: None,
            note: None,
            project_root: project_root.into(),
            wip: None,
        }
    }

    #[test]
    fn sequence_numbers_follow_creation_order_not_snapshot_order() {
        // Snapshot is newest-first by updated_at; the dock must re-sort by
        // creation so #1 is the oldest session no matter how rows arrive.
        let rows = vec![
            row("c-newest", 300, "/work/app", SessionStatus::Running),
            row("a-oldest", 100, "/work/lib", SessionStatus::Idle),
            row("b-middle", 200, "/work/web", SessionStatus::Idle),
        ];
        let entries = dock_entries(&rows);
        // dock_entries preserves input order as its slots but stamps seq by
        // creation: a-oldest must be #1 even though it sits at index 1.
        let seq_of = |id: &str| entries.iter().find(|e| e.row.id == id).unwrap().seq;
        assert_eq!(seq_of("a-oldest"), 1);
        assert_eq!(seq_of("b-middle"), 2);
        assert_eq!(seq_of("c-newest"), 3);
        // And the raw creation_order mapping (used by event handlers) agrees.
        let order = creation_order(&rows);
        assert_eq!(order, vec![1, 2, 0]);
    }

    #[test]
    fn creation_order_is_stable_for_equal_timestamps() {
        let rows = vec![
            row("b", 100, "/p/b", SessionStatus::Idle),
            row("a", 100, "/p/a", SessionStatus::Idle),
        ];
        assert_eq!(creation_order(&rows), vec![1, 0]);
        // Repeated calls give the same answer (total order via id tiebreak).
        assert_eq!(creation_order(&rows), creation_order(&rows));
    }

    #[test]
    fn workspace_names_use_basename_until_they_collide() {
        let rows = vec![
            row("one", 1, "/home/ming/projects/neenee", SessionStatus::Idle),
            row("two", 2, "/home/ming/projects/app", SessionStatus::Idle),
        ];
        let entries = dock_entries(&rows);
        assert_eq!(entries[0].workspace, "neenee");
        assert_eq!(entries[1].workspace, "app");

        // Same basename in two different parents → both fall back to full
        // paths (a bare "src" would be ambiguous AND meaningless).
        let rows = vec![
            row("one", 1, "/home/ming/projects/neenee", SessionStatus::Idle),
            row("two", 2, "/tmp/worktree/neenee", SessionStatus::Idle),
        ];
        let entries = dock_entries(&rows);
        assert_eq!(entries[0].workspace, "/home/ming/projects/neenee");
        assert_eq!(entries[1].workspace, "/tmp/worktree/neenee");
    }

    #[test]
    fn workspace_basename_never_shows_a_parent() {
        assert_eq!(workspace_basename("/home/ming/projects/neenee"), "neenee");
        assert_eq!(workspace_basename("/home/ming/projects/neenee/"), "neenee");
        assert_eq!(workspace_basename("/"), "/");
        assert_eq!(workspace_basename("/src"), "src");
    }

    #[test]
    fn dock_columns_adapt_to_width() {
        assert_eq!(dock_columns(30), 1);
        assert_eq!(dock_columns(36), 1);
        assert_eq!(dock_columns(72), 2);
        assert_eq!(dock_columns(140), 3);
        assert_eq!(dock_columns(400), 4); // capped
    }

    #[test]
    fn dock_card_shows_seq_workspace_uptime_and_status() {
        let theme = Theme::default();
        let now = 1_000_000u64;
        let r = row(
            "x",
            now - (2 * 3600 + 14 * 60),
            "/work/neenee",
            SessionStatus::Running,
        );
        let entries = dock_entries(std::slice::from_ref(&r));
        let line = dock_card_line(&entries[0], 60, false, false, now, &theme);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("#1"), "{text}");
        assert!(text.contains("neenee"), "{text}");
        assert!(text.contains("2h14m"), "{text}");
        assert!(text.contains("running"), "{text}");
    }

    #[test]
    fn dock_card_truncates_long_workspace_names() {
        let theme = Theme::default();
        let now = 1_000u64;
        // Colliding basenames force the full path onto the card; a long full
        // path must then be ellipsized into the name field.
        let rows = vec![
            row(
                "a",
                1,
                "/very/long/path/to/a/deeply/nested/project",
                SessionStatus::Idle,
            ),
            row("b", 2, "/other/worktree/project", SessionStatus::Idle),
        ];
        let entries = dock_entries(&rows);
        let line = dock_card_line(&entries[0], 60, false, false, now, &theme);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains('…'), "{text}");
        assert!(
            unicode_width::UnicodeWidthStr::width(text.as_str()) <= 60,
            "{text:?}"
        );
        // Idle reads as "done" in the dock vocabulary.
        assert!(text.contains("done"), "{text}");
        // The unique-basename sibling is untouched.
        assert_eq!(entries[1].workspace, "/other/worktree/project");
    }

    #[test]
    fn dock_card_at_minimum_width_keeps_fixed_fields() {
        let theme = Theme::default();
        let now = 1_000u64;
        let r = row("x", now, "/work/neenee", SessionStatus::Running);
        let entries = dock_entries(std::slice::from_ref(&r));
        // A degenerate 30-cell cell (below the 36 target): seq, uptime and
        // status all survive; the name field just gets cramped.
        let line = dock_card_line(&entries[0], 30, false, false, now, &theme);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("#1"), "{text}");
        assert!(text.contains("running"), "{text}");
    }

    #[test]
    fn uptime_format_scales() {
        assert_eq!(format_uptime(43), "43s");
        assert_eq!(format_uptime(12 * 60), "12m");
        assert_eq!(format_uptime(2 * 3600 + 14 * 60), "2h14m");
        assert_eq!(format_uptime(3 * 86_400 + 5 * 3600), "3d5h");
    }

    #[test]
    fn dock_status_vocabulary() {
        assert_eq!(dock_status_label(SessionStatus::Running), "running");
        assert_eq!(dock_status_label(SessionStatus::Idle), "done");
        assert_eq!(
            dock_status_label(SessionStatus::NeedsApproval),
            "needs approval"
        );
        assert_eq!(dock_status_label(SessionStatus::NeedsInput), "needs input");
        assert_eq!(dock_status_label(SessionStatus::Failed), "failed");
    }
}
