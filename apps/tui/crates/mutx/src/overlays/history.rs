//! History search panel (Ctrl+R).
//!
//! A floating dropdown panel anchored above the composer. Unlike a centered
//! modal, the composer itself stays live: it becomes the filter input for the
//! panel, so typing immediately narrows the list below. The panel lists the
//! **entire cross-session history**, newest-first — Ctrl+R deliberately
//! ignores which session or workspace an entry came from, since its whole
//! purpose is global recall. The inline ↑/↓ recall, by contrast, is scoped
//! to the current session (see `App::current_session_history`).
//!
//! Each row is a single line (multi-line prompts collapse to the first line
//! with a `↵` marker); the row numbers + prompt text are enough to navigate,
//! so there is no origin status strip — the `~/project · #session… · time`
//! line was redundant noise and was removed. `Ctrl+X` clears the whole
//! history (with an explicit `y` confirm), `Enter` inserts the focused entry
//! into the composer, `Tab` previews the full multi-line text.

use muta_contracts::HistoryEntry;
use mutx_engine::{
    Block as RtBlock, Clear as RtClear, Frame, Modifier, Paragraph, Rect, Style, {Line, Span},
};

use super::common::truncate_ellipsis;
use crate::fuzzy::FuzzyMatch;
use crate::primitives::{
    FooterHint, SCROLL_EDGE_MARGIN, contrast_fg, keymap_body_lines, keyvocab, render_body,
    render_modal_footer_with_more,
};
use crate::view::Theme;

/// Maximum number of rows the dropdown reserves vertically. Capped so a long
/// history stays scannable — a Ctrl+R picker is a glance surface, not a full
/// browser — and the body scrolls within this budget. Ten entries is enough to
/// recall a recent prompt at a glance; anything older is a search away (the
/// composer below is the live filter field).
const HISTORY_PANEL_MAX_ROWS: u16 = 10;

/// Draw the history search dropdown, anchored above `input_rect`.
///
/// `ranked` is the pre-computed `(original_history_index, FuzzyMatch)` list
/// produced by `App::history_rows` — passing it in avoids a second fuzzy pass
/// per frame. `modal_index` selects into `ranked`. `scroll` is read AND
/// written back so the caller's offset stays consistent with the clamped body
/// height; `follow_selection` gates whether the body auto-scrolls to keep
/// `modal_index` in view (true after navigation, false once the user scrolls
/// manually). `preview` switches the body from the one-line fuzzy list to a
/// full-text view of the selected entry (toggled by Tab); `scroll` is reused
/// as that entry's per-line scroll. `keymap_open` replaces the body with the
/// in-panel keybindings list (the `?` expand).
///
/// The panel floats with `Recess::None` (no dimming), and the composer below
/// stays fully live as the filter field — the caller still renders it.
///
/// `activity_height` is the row count the transient activity bar occupies in
/// the row(s) immediately above the composer this frame (0 when the bar is
/// hidden). The dropdown treats the activity bar's bottom edge as an upper
/// bound: it never grows into the activity bar's rows, so the activity bar is
/// always visible and always reads as above the history dropdown. This keeps
/// the dropdown an extension of the composer rather than something that can
/// occlude the live status surface above it.
///
/// The returned rect is the panel's footprint (for click-outside-dismiss hit
/// testing); it is `None` when there is no room above the activity bar
/// (height 0), in which case nothing is drawn.
#[allow(clippy::too_many_arguments)]
pub fn draw_history_panel(
    frame: &mut Frame,
    history: &[HistoryEntry],
    ranked: &[(usize, FuzzyMatch)],
    modal_index: usize,
    scroll: &mut usize,
    follow_selection: bool,
    preview: bool,
    keymap_open: bool,
    input_rect: Rect,
    activity_height: u16,
    theme: &Theme,
    selection: &crate::model::selection::SelectionState,
    layout_map: &mut crate::model::layout::LayoutMap,
) -> Option<Rect> {
    // Compute the panel footprint: it grows upward from the top edge of the
    // composer. The activity bar sits flush above the composer, so reserve
    // its rows: the dropdown's ceiling is the activity bar's top edge, never
    // the composer's top edge — it must never paint over the live status bar
    // above it. Height tracks the actual content (one row per entry) floored
    // at a single body row so an empty/short history reads as a sliver rather
    // than a fixed-size box, capped at the max so a huge history scrolls
    // instead of eating the whole screen.
    let activity_h = activity_height.min(input_rect.y);
    let area_top = input_rect.y.saturating_sub(activity_h);
    let room_above = area_top;
    let row_count = ranked.len().max(1) as u16;
    let desired_rows = row_count.min(HISTORY_PANEL_MAX_ROWS);
    // +1 header (title), +1 footer (origin strip + key hints), +2 composer
    // chrome (the full panel-bg top/bottom padding rows the panel shares with
    // the composer below it).
    const CHROME_ROWS: u16 = 4;
    let desired_h = desired_rows.saturating_add(CHROME_ROWS);
    let panel_h = desired_h.min(room_above);
    if panel_h == 0 {
        return None;
    }
    // The panel grows upward from the activity bar's top edge (its reserved
    // ceiling), never from the composer's top edge: this is what keeps it out
    // of the activity bar's rows. Its footprint is [area_top - panel_h, area_top).
    let panel_y = area_top.saturating_sub(panel_h);
    let area = Rect::new(input_rect.x, panel_y, input_rect.width, panel_h);

    // The panel shares the composer's surface language so the dropdown reads as
    // an extension of the input box rather than a separate floating window: a
    // solid `panel()` fill with full panel-bg padding rows on the top and
    // bottom edges, so it breathes exactly like the composer does. No left
    // accent bar — the composer has none, and a full-height brand column would
    // read as selection/severity, which a history list is not. The edges are
    // painted by the same panel fill (no half-block `▄`/`▀` glyphs), so the
    // transition is identical across terminals.
    let panel_bg = theme.panel();
    let inner_w = area.width;
    frame.render_widget(RtClear, area);
    frame.render_widget(
        RtBlock::default().style(Style::default().bg(panel_bg)),
        area,
    );

    // Header row: title + live query echo + counts. Sits just inside the top
    // transition row, full width (no left-accent column to inset around).
    let header_rect = Rect::new(area.x, area.y + 1, inner_w, 1);
    let footer_rect = Rect::new(area.x, area.y + area.height.saturating_sub(2), inner_w, 1);
    // Body sits between header and footer.
    let body_rect = if area.height >= CHROME_ROWS {
        Rect::new(
            area.x,
            header_rect.y + 1,
            inner_w,
            area.height.saturating_sub(CHROME_ROWS),
        )
    } else {
        // Degenerate tiny terminal: give the body whatever is left after the
        // top transition + header so the list is still visible.
        Rect::new(
            area.x,
            header_rect.y + 1,
            inner_w,
            area.height.saturating_sub(2),
        )
    };

    draw_header(frame, header_rect, history.len(), ranked.len(), theme);

    if keymap_open {
        let hints: [FooterHint; 4] = [
            FooterHint::navigation(keyvocab::ARROWS_UD, "next entry"),
            FooterHint::key_primary(crate::keymap::Key::ENTER, "insert"),
            FooterHint::key_secondary(crate::keymap::Key::TAB, "preview"),
            FooterHint::key_always(crate::keymap::Key::ESC, "close"),
        ];
        let body = keymap_body_lines(&hints, &[], theme);
        // Selectable document: the keymap sub-page registers as MODAL_DOC
        // rows so key labels and descriptions are copyable.
        let rows: Vec<crate::components::selectable_body::SelectableRow> = body
            .into_iter()
            .map(crate::components::selectable_body::SelectableRow::from_line)
            .collect();
        crate::components::selectable_body::render_selectable_body(
            frame, body_rect, &rows, scroll, None, theme, selection, layout_map,
        );
        render_modal_footer_with_more(frame, footer_rect, &hints, &[], theme);
        return Some(area);
    }

    if preview {
        // Selectable document: the full text of the focused history entry is
        // exactly what a user would want to copy (e.g. to re-use elsewhere).
        let body = preview_body(history, ranked, modal_index, theme);
        let rows: Vec<crate::components::selectable_body::SelectableRow> = body
            .into_iter()
            .map(crate::components::selectable_body::SelectableRow::from_line)
            .collect();
        crate::components::selectable_body::render_selectable_body(
            frame, body_rect, &rows, scroll, None, theme, selection, layout_map,
        );
    } else {
        let body = list_body(
            history,
            ranked,
            modal_index,
            theme,
            body_rect.width as usize,
        );
        let follow = follow_selection.then_some(modal_index);
        render_body(
            frame,
            body_rect,
            body,
            scroll,
            crate::primitives::BodyRenderOptions::new(follow, SCROLL_EDGE_MARGIN, false),
            theme,
        );
    }

    // Footer: the key hints strip (right-aligned). The selected row's origin
    // strip was removed as redundant — see [`draw_footer`].
    draw_footer(frame, footer_rect, theme);

    Some(area)
}

/// Header: `History` title, the count of visible/total.
fn draw_header(frame: &mut Frame, rect: Rect, total: usize, shown: usize, theme: &Theme) {
    let title = Span::styled(
        "History",
        Style::default()
            .fg(theme.brand())
            .add_modifier(Modifier::BOLD),
    );
    let count = if total == 0 {
        Span::styled("  no history yet", Style::default().fg(theme.muted()))
    } else if shown == total {
        Span::styled(format!("  {shown}"), Style::default().fg(theme.muted()))
    } else {
        Span::styled(
            format!("  {shown}/{total}"),
            Style::default().fg(theme.muted()),
        )
    };
    frame.render_widget(Paragraph::new(Line::from(vec![title, count])), rect);
}

/// Footer: the key hints strip (right-aligned). The selected row's origin
/// (workspace · session · time) used to trail on the left, but that line was
/// redundant noise — the prompt text itself is the entry, and the row numbers
/// already anchor selection — so it was removed. When the `?` keymap page is
/// open the caller draws its own footer, so this is only the normal-mode strip.
fn draw_footer(frame: &mut Frame, rect: Rect, theme: &Theme) {
    let hints: [FooterHint; 5] = [
        FooterHint::secondary("type", "filter"),
        FooterHint::navigation(keyvocab::ARROWS_UD, "navigate"),
        FooterHint::key_primary(crate::keymap::Key::ENTER, "insert"),
        FooterHint::key_always(crate::keymap::Key::CTRL_X, "clear"),
        FooterHint::key_always(crate::keymap::Key::ESC, "close"),
    ];
    render_modal_footer_with_more(frame, rect, &hints, &[], theme);
}

/// Build the one-line-per-entry fuzzy list body. Multi-line entries are
/// collapsed to their first line with a trailing ` ↵` marker so a long prompt
/// never breaks the single-row grid; the full text is one Tab away.
fn list_body<'a>(
    history: &'a [HistoryEntry],
    ranked: &'a [(usize, FuzzyMatch)],
    modal_index: usize,
    theme: &Theme,
    body_width: usize,
) -> Vec<Line<'static>> {
    let mut body: Vec<Line> = Vec::new();
    if history.is_empty() {
        body.push(Line::from(""));
        body.push(Line::from(Span::styled(
            " (no history yet — send a message to populate this list)",
            Style::default().fg(theme.muted()),
        )));
        return body;
    }
    if ranked.is_empty() {
        body.push(Line::from(""));
        body.push(Line::from(Span::styled(
            " (no matches — try a shorter or different query)",
            Style::default().fg(theme.muted()),
        )));
        return body;
    }

    // Row-number prefix " 123 " = 6 columns; the " ↵" continuation marker is
    // reserved 2 columns only when actually appended.
    const ROW_NUM_COLS: usize = 6;
    for (row, (orig_idx, m)) in ranked.iter().enumerate() {
        let is_selected = row == modal_index;
        let bg = if is_selected {
            theme.brand()
        } else {
            theme.panel()
        };
        let fg = if is_selected {
            contrast_fg(theme.brand())
        } else {
            theme.fg()
        };
        let num_style = if is_selected {
            Style::default().bg(bg).fg(contrast_fg(theme.brand()))
        } else {
            Style::default().fg(theme.muted())
        };
        let base_style = Style::default().bg(bg).fg(fg);
        let matched_style = if is_selected {
            Style::default()
                .bg(bg)
                .fg(contrast_fg(theme.brand()))
                .add_modifier(Modifier::UNDERLINED)
        } else {
            Style::default()
                .bg(bg)
                .fg(theme.brand())
                .add_modifier(Modifier::BOLD)
        };

        let raw = history
            .get(*orig_idx)
            .map(|e| e.text.as_str())
            .unwrap_or("");
        // Collapse to a single line: take the first physical line and mark
        // continuation so a multi-line prompt reads as one row. The highlight
        // positions (computed against `raw`) map onto the first line since any
        // character past the first `\n` is dropped before truncation.
        let (first_line, multiline) = match raw.find('\n') {
            Some(i) => (&raw[..i], true),
            None => (raw, false),
        };
        // Reserve room for the continuation glyph before truncating so it
        // never lands outside the panel edge.
        let reserve = if multiline { 2 } else { 0 };
        let entry_max = body_width.saturating_sub(ROW_NUM_COLS + reserve);
        let entry = truncate_ellipsis(first_line, entry_max);
        let matched: std::collections::HashSet<usize> = m
            .positions
            .iter()
            .copied()
            .filter(|&p| p <= first_line.len())
            .collect();

        let mut spans: Vec<Span> = Vec::with_capacity(entry.chars().count() + 2);
        spans.push(Span::styled(format!(" {:>3} ", row + 1), num_style));
        for (char_idx, c) in entry.chars().enumerate() {
            let style = if matched.contains(&char_idx) {
                matched_style
            } else {
                base_style
            };
            spans.push(Span::styled(c.to_string(), style));
        }
        if multiline {
            spans.push(Span::styled(" ↵", Style::default().bg(bg).fg(num_style.fg)));
        }
        body.push(Line::from(spans));
    }
    body
}

/// Build the full-text preview body for the focused entry. The entry is laid
/// out verbatim (one `Line` per physical line) with the fuzzy-match positions
/// highlighted on whichever lines they fall; ↑/↓ move to the next entry and
/// the renderer re-anchors its own scroll to the top.
fn preview_body(
    history: &[HistoryEntry],
    ranked: &[(usize, FuzzyMatch)],
    modal_index: usize,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let Some((orig_idx, m)) = ranked.get(modal_index) else {
        return vec![Line::from(Span::styled(
            " (no entry selected)",
            Style::default().fg(theme.muted()),
        ))];
    };
    let raw = history
        .get(*orig_idx)
        .map(|e| e.text.as_str())
        .unwrap_or("");
    let matched: std::collections::HashSet<usize> = m.positions.iter().copied().collect();

    let body_style = Style::default().fg(theme.fg());
    let matched_style = Style::default()
        .fg(theme.brand())
        .add_modifier(Modifier::BOLD);

    let mut lines: Vec<Line> = Vec::new();
    let mut char_idx = 0usize;
    for line in raw.split('\n') {
        let mut spans: Vec<Span> = Vec::with_capacity(line.chars().count());
        for c in line.chars() {
            let style = if matched.contains(&char_idx) {
                matched_style
            } else {
                body_style
            };
            spans.push(Span::styled(c.to_string(), style));
            char_idx += 1;
        }
        lines.push(Line::from(spans));
        char_idx += 1; // the consumed `\n`
    }
    lines
}
