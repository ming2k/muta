//! Permission sheet (inline) and question modal.

use neenee_tui_engine::{
    Frame, Rect, {Block as RtBlock, Clear, Paragraph}, {Line, Span}, {Modifier, Style},
};

use neenee_contracts::{PermissionRequest, UserQuestionRequest};

use crate::components::options::{ChoiceMarker, ChoiceOptionRow, ChoiceTone, push_wrapped_styled};
use crate::model::layout::{ModalHitMap, PermissionActionHit, QuestionOptionHit};
use crate::primitives::{
    FixedModalSpec, FooterHint, contrast_fg, keyvocab, modal_area, modal_footer_text, modal_frame,
    panel_block, render_body, render_modal_footer,
};
use crate::text_layout::wrap_text;
use crate::view::Theme;
use unicode_width::UnicodeWidthStr;

// The permission sheet renders inline, replacing the composer (input box)
// area. Collapsed it shows a one-line summary plus the action footer;
// expanding "Details" grows the body upward into the transcript.
const PERMISSION_H_PADDING: u16 = 2;
const PERMISSION_TOP_PADDING: u16 = 1;
const PERMISSION_FOOTER_HEIGHT: u16 = 1;
const PERMISSION_BODY_FOOTER_GAP: u16 = 1;
/// Max body rows in the collapsed (summary-only) state.
const PERMISSION_COLLAPSED_BODY_CAP: u16 = 2;
/// Max body rows when "Details" is expanded; the rest is scrollable.
const PERMISSION_MAX_BODY_ROWS: u16 = 14;

/// options; the user navigates with ↑/↓, selects with Space, and advances with
/// Enter. Multi-select questions use checkboxes; single-select
/// shows no marker at all — the highlight *is* the selection (it moves live
/// with ↑/↓ and a digit jump). Enter advances to the next question or submits
/// all answers on the final page. A numbered digit key (1-9) jumps directly to
/// an option; Shift+Tab returns to the previous question.
const OTHER_OPTION_LABEL: &str = "Other";

#[allow(clippy::too_many_arguments)] // modal draw fns thread many context args by nature
pub fn draw_question_modal(
    frame: &mut Frame,
    hit_map: &mut ModalHitMap,
    request: &UserQuestionRequest,
    current_question: usize,
    selected: &[Vec<usize>],
    other_text: &[String],
    highlighted: usize,
    scroll: &mut usize,
    follow_highlight: bool,
    theme: &Theme,
) -> neenee_tui_engine::Rect {
    let area = modal_area(frame, FixedModalSpec::QUESTION);
    let f = modal_frame(frame, area, theme.panel(), true, true);

    let question = request.questions.get(current_question);
    let total = request.questions.len();

    if let Some(h) = f.header {
        let title = if total > 1 {
            format!("Question {}/{}", current_question + 1, total)
        } else {
            "Question".to_string()
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                title,
                Style::default()
                    .fg(theme.brand())
                    .add_modifier(Modifier::BOLD),
            ))),
            h,
        );
    }

    let mut body_lines: Vec<Line> = Vec::new();
    let mut option_rows: Vec<(usize, usize, usize)> = Vec::new();
    let body_width = f.body.width as usize;
    let mut highlighted_row = None;
    // Body row index + column of the "Other" free-text field's caret,
    // captured only while "Other" is highlighted. Unlike a plain list row,
    // the field can span several wrapped lines, so both the body-scroll
    // follow target and the real terminal cursor position refer to the
    // *caret row* (last wrapped line), not the "Other" label row —
    // otherwise a multi-line field leaves the caret scrolled out of view.
    let mut other_caret_row: Option<usize> = None;
    let mut other_caret_col: usize = 0;
    // 5-column indent of the "Other" free-text field, matching the `"     "`
    // prefix passed to `push_wrapped_styled`.
    const OTHER_FIELD_INDENT: usize = 5;
    if let Some(q) = question {
        if let Some(header) = &q.header {
            push_wrapped_styled(
                &mut body_lines,
                "",
                "",
                header,
                Style::default()
                    .fg(theme.info())
                    .add_modifier(Modifier::BOLD),
                body_width,
            );
        }
        push_wrapped_styled(
            &mut body_lines,
            "",
            "",
            &q.question,
            Style::default().fg(theme.fg()),
            body_width,
        );
        body_lines.push(Line::from(""));

        let q_selected = selected.get(current_question);
        let other_index = q.options.len();
        let other_highlighted = highlighted == other_index;
        let other_text_value = other_text
            .get(current_question)
            .map(String::as_str)
            .unwrap_or("");

        for (i, option) in q.options.iter().enumerate() {
            let is_selected = q_selected.is_some_and(|s| s.contains(&i));
            let is_highlighted = i == highlighted;
            let row = body_lines.len();
            if is_highlighted {
                highlighted_row = Some(row);
            }
            let start = body_lines.len();
            render_question_option(
                &mut body_lines,
                i,
                &option.label,
                option.description.as_deref(),
                is_selected,
                is_highlighted,
                q.multi_select,
                body_width,
                theme,
            );
            option_rows.push((i, start, body_lines.len()));
        }

        let row = body_lines.len();
        if other_highlighted {
            highlighted_row = Some(row);
        }
        let other_start = body_lines.len();
        render_question_option(
            &mut body_lines,
            other_index,
            OTHER_OPTION_LABEL,
            None,
            q_selected.is_some_and(|s| s.contains(&other_index)),
            other_highlighted,
            q.multi_select,
            body_width,
            theme,
        );
        // The free-text field row sits directly beneath the "Other" option
        // line. We render the typed text *without* a trailing `█` glyph: the
        // terminal's own block cursor (placed below via `set_cursor_position`)
        // is the caret, which is what the host IME samples to anchor its
        // composition window. A painted glyph would be a fake cursor the IME
        // cannot see.
        if other_highlighted {
            let field_start_row = body_lines.len();
            push_wrapped_styled(
                &mut body_lines,
                "     ",
                "     ",
                other_text_value,
                Style::default().fg(theme.brand()),
                body_width,
            );
            // Resolve the caret location through the *same* `wrap_text` pass
            // the renderer used (same indent budget) so the body-scroll follow
            // target and the cursor placement both point at the caret's real
            // visual row + column. The field is append-only, so the caret is
            // always at the end of the text: last wrapped row, end column.
            let wrap_budget = body_width.saturating_sub(OTHER_FIELD_INDENT).max(1);
            let wrapped = wrap_text(other_text_value, wrap_budget);
            let wrapped_rows = wrapped.len().max(1);
            let caret_local_col = wrapped
                .last()
                .map(|wl| neenee_tui_engine::text::cursor_column(&wl.text, wl.text.len()))
                .unwrap_or(0);
            other_caret_row = Some(field_start_row + wrapped_rows.saturating_sub(1));
            other_caret_col = caret_local_col;
        }
        option_rows.push((other_index, other_start, body_lines.len()));
    }

    // Auto-follow the highlight only while navigating (the default after open /
    // ↑↓ / digit-jump); a manual wheel/page scroll clears the flag so the user
    // can browse a long question or option list without the body snapping back
    // to the cursor. Mirrors the session / history modals.
    //
    // When the "Other" free-text field is active, the caret can sit several
    // rows below the "Other" label (the field wraps), so follow the *caret*
    // row instead of the label row — otherwise typing past the first line
    // scrolls the caret out of view.
    let follow_target = other_caret_row.or(highlighted_row);
    let follow = if follow_highlight {
        follow_target
    } else {
        None
    };
    render_body(frame, f.body, body_lines, scroll, follow, 0, false, theme);
    record_question_hits(hit_map, f.body, &option_rows, *scroll);

    // Place the real terminal cursor in the "Other" free-text field — the only
    // text-input surface in this modal. This is what the host IME samples to
    // anchor its composition window; without it, IME-based input (CJK, etc.)
    // cannot bind to the field. The field's 5-column indent matches the
    // `"     "` prefix passed to `push_wrapped_styled`, and the caret sits at
    // the end of the typed text (the field is append-only, so the caret is
    // always at the end).
    //
    // The caret row was resolved through the *same* `wrap_text` pass used for
    // the follow target above, and `follow` has already nudged `scroll` to
    // keep it on screen. We still guard by the visible window: if the field is
    // scrolled away (e.g. the user is browsing with wheel/Pg), there is no
    // honest coordinate and the event loop leaves the cursor hidden.
    if let Some(caret_row) = other_caret_row {
        let visible_top = *scroll;
        let visible_bottom = scroll.saturating_add(f.body.height as usize);
        if caret_row >= visible_top && caret_row < visible_bottom {
            let indent: u16 = OTHER_FIELD_INDENT as u16;
            let cursor_x = f.body.x + indent + other_caret_col as u16;
            let cursor_y = f.body.y + (caret_row - visible_top) as u16;
            // Clamp to the body's right edge so a wide-glyph caret at the last
            // column never lands in the scrollbar gutter.
            let cursor_x = cursor_x.min(f.body.x + f.body.width.saturating_sub(1));
            frame.set_cursor_position((cursor_x, cursor_y));
        }
    }

    if let Some(fo) = f.footer {
        // Single-select is live (the highlight is the selection), so there is
        // no "select" action to advertise — Space is a no-op there. Only
        // multi-select offers the Space toggle.
        let enter_label = if current_question + 1 < total {
            "next"
        } else {
            "submit"
        };
        let mut hints = vec![
            FooterHint::navigation(keyvocab::ARROWS_UD, "navigate"),
            FooterHint::navigation("wheel/Pg", "scroll"),
            FooterHint::primary(keyvocab::ENTER, enter_label),
        ];
        if current_question > 0 {
            hints.push(FooterHint::secondary(keyvocab::SHIFT_TAB, "back"));
        }
        if question.is_some_and(|q| q.multi_select) {
            hints.push(FooterHint::secondary(keyvocab::SPACE, "select"));
        }
        hints.push(FooterHint::secondary("1-9", "jump"));
        hints.push(FooterHint::always(keyvocab::ESC, "cancel"));
        render_modal_footer(frame, fo, &hints, theme);
    }
    area
}

fn record_question_hits(
    hit_map: &mut ModalHitMap,
    body: Rect,
    option_rows: &[(usize, usize, usize)],
    scroll: usize,
) {
    if body.width == 0 || body.height == 0 {
        return;
    }
    let visible_top = scroll;
    let visible_bottom = scroll + body.height as usize;
    for &(option_index, start, end) in option_rows {
        let top = start.max(visible_top);
        let bottom = end.max(start + 1).min(visible_bottom);
        if top >= bottom {
            continue;
        }
        hit_map.push_question_option(QuestionOptionHit {
            option_index,
            rect: Rect::new(
                body.x,
                body.y + (top - visible_top) as u16,
                body.width,
                (bottom - top) as u16,
            ),
        });
    }
}

#[allow(clippy::too_many_arguments)]
fn render_question_option(
    lines: &mut Vec<Line<'static>>,
    _index: usize,
    label: &str,
    description: Option<&str>,
    is_selected: bool,
    is_highlighted: bool,
    multi_select: bool,
    body_width: usize,
    theme: &Theme,
) {
    ChoiceOptionRow {
        label,
        description,
        selected: is_selected,
        highlighted: is_highlighted,
        tone: ChoiceTone::Flat,
        marker: if multi_select {
            ChoiceMarker::Checkbox
        } else {
            ChoiceMarker::None
        },
    }
    .push_lines(lines, body_width, theme);
}

/// Draw a blocking tool permission request inline, replacing the composer
/// (input box) area. The transcript above stays visible and scrollable.
///
/// Collapsed (the default) the sheet is a one-line summary — the tool name
/// plus its scope (the specific path/command being touched) — followed by a
/// footer of inline actions. Selecting "Details" expands the body upward to
/// reveal the full description and arguments without leaving the prompt.
#[allow(clippy::too_many_arguments)]
pub fn draw_permission_sheet(
    frame: &mut Frame,
    hit_map: &mut ModalHitMap,
    request: &PermissionRequest,
    selected: usize,
    confirm_always: bool,
    show_details: bool,
    scroll: usize,
    input_rect: Rect,
    theme: &Theme,
    selection: &crate::model::selection::SelectionState,
    layout_map: &mut crate::model::layout::LayoutMap,
) -> usize {
    let area_bottom = input_rect.y + input_rect.height;

    let arguments = serde_json::from_str::<serde_json::Value>(&request.arguments)
        .ok()
        .and_then(|value| serde_json::to_string_pretty(&value).ok())
        .unwrap_or_else(|| request.arguments.clone());
    let scope_meaningful = !request.scope.is_empty() && request.scope != "*";

    // Header line: human-friendly label (falling back to the raw tool name
    // for safety), plus the concrete scope (path/command) when it adds
    // information. The scope is the single most useful detail, so it earns a
    // spot in the collapsed summary; everything else hides behind "Details".
    // The left bar carries the severity cue.
    let label = if request.label.is_empty() {
        request.tool.clone()
    } else {
        request.label.clone()
    };
    let mut header = vec![Span::styled(
        label,
        Style::default()
            .fg(theme.brand())
            .add_modifier(Modifier::BOLD),
    )];
    // #10: an elevation prompt (out-of-scope target) is flagged ⚠ so the
    // operator understands they are authorising access *beyond* the configured
    // boundary, not a routine in-scope call. Rendered in the error colour.
    if request.elevation {
        header.push(Span::styled("  ", Style::default()));
        header.push(Span::styled(
            "⚠ out of scope",
            Style::default()
                .fg(theme.err())
                .add_modifier(Modifier::BOLD),
        ));
    }
    if confirm_always {
        header.push(Span::styled(
            " — always allow until exit?",
            Style::default().fg(theme.fg()),
        ));
    } else if request.one_off {
        // A one-off dangerous-command confirm: flag that this grant will not be
        // remembered, so the user is not surprised to be re-prompted next time.
        header.push(Span::styled(
            " — one-off (not remembered)",
            Style::default().fg(theme.warn()),
        ));
    } else if scope_meaningful {
        header.push(Span::styled("  ", Style::default()));
        header.push(Span::styled(
            request.scope.clone(),
            Style::default().fg(theme.info()),
        ));
    }

    let mut body_lines: Vec<Line> = Vec::new();
    body_lines.push(Line::from(header));

    if confirm_always {
        body_lines.push(Line::from(Span::styled(
            "Grants this tool until neenee exits.",
            Style::default().fg(theme.muted()),
        )));
    } else if show_details {
        body_lines.push(Line::from(""));
        body_lines.push(Line::from(Span::styled(
            request.description.clone(),
            Style::default().fg(theme.fg()),
        )));
        body_lines.push(Line::from(""));
        body_lines.push(Line::from(Span::styled(
            "Arguments",
            Style::default()
                .fg(theme.info())
                .add_modifier(Modifier::BOLD),
        )));
        body_lines.extend(arguments.lines().map(|line| {
            Line::from(Span::raw(line.to_string())).style(Style::default().fg(theme.code_text()))
        }));
    }

    let fixed = PERMISSION_TOP_PADDING + PERMISSION_BODY_FOOTER_GAP + PERMISSION_FOOTER_HEIGHT;
    let content_w = input_rect
        .width
        .saturating_sub(1 + 2 * PERMISSION_H_PADDING)
        .max(1);
    let body_total_rows: usize = body_lines
        .iter()
        .map(|line| {
            let width: usize = line.spans.iter().map(|span| span.content.width()).sum();
            width.max(1).div_ceil(content_w as usize)
        })
        .sum();

    // How tall the body may grow. Collapsed stays compact; expanded climbs
    // into the transcript but never past the top of the viewport.
    let body_cap: u16 = if confirm_always {
        body_total_rows.min(2).min(u16::MAX as usize) as u16
    } else if show_details {
        let room = area_bottom.saturating_sub(fixed).max(1);
        PERMISSION_MAX_BODY_ROWS.min(room)
    } else {
        PERMISSION_COLLAPSED_BODY_CAP
    };
    let body_h = (body_total_rows as u16).min(body_cap);
    let max_scroll = body_total_rows.saturating_sub(body_h as usize);
    let body_scroll = scroll.min(max_scroll);

    let desired_h = fixed + body_h;
    // Fill the composer slot when collapsed (so it reads as a drop-in
    // replacement for the input box); grow above it when expanded.
    let sheet_h = desired_h.max(input_rect.height).min(area_bottom).max(1);
    let sheet_top = area_bottom.saturating_sub(sheet_h);
    let area = Rect::new(input_rect.x, sheet_top, input_rect.width, sheet_h);
    hit_map.set_permission_sheet(area);

    frame.render_widget(Clear, area);
    frame.render_widget(panel_block(theme.warn(), theme.panel()), area);

    let content_x = area.x + 1 + PERMISSION_H_PADDING;
    let body_area = Rect::new(
        content_x,
        area.y + PERMISSION_TOP_PADDING,
        content_w,
        body_h,
    );
    // Selectable document: the tool-call arguments JSON (and the description
    // above it) is exactly what a user wants to copy while deciding. The
    // body's line-level scroll (`body_scroll` counts wrapped visual rows,
    // same accounting `resolve_scroll` uses) is passed straight through.
    let rows: Vec<crate::components::selectable_body::SelectableRow> = body_lines
        .into_iter()
        .map(crate::components::selectable_body::SelectableRow::from_line)
        .collect();
    let mut body_scroll_usize = body_scroll;
    crate::components::selectable_body::render_selectable_body(
        frame,
        body_area,
        &rows,
        &mut body_scroll_usize,
        None,
        theme,
        selection,
        layout_map,
    );

    let footer_y = area
        .y
        .saturating_add(sheet_h)
        .saturating_sub(PERMISSION_FOOTER_HEIGHT);
    let footer_band = Rect::new(
        area.x + 1,
        footer_y,
        area.width.saturating_sub(1),
        PERMISSION_FOOTER_HEIGHT,
    );
    frame.render_widget(
        RtBlock::default().style(Style::default().bg(theme.raised())),
        footer_band,
    );

    let details_label = if show_details { "Hide" } else { "Details" };
    // #2: a one-off prompt (the bash dangerous-command confirm) deliberately
    // does not persist an `Always` reply, so the "Always allow" option is
    // suppressed entirely — offering a button whose choice is silently ignored
    // is a UI/behaviour lie. The decision collapses to Allow once / Reject /
    // Details. (The confirm_always keyboard shortcut is also inert for these
    // prompts, since there is no Always choice to confirm.)
    let labels: Vec<&str> = if confirm_always && !request.one_off {
        vec!["Confirm always", "Cancel"]
    } else if request.one_off {
        vec!["Allow once", "Reject", details_label]
    } else {
        vec!["Allow once", "Always allow", "Reject", details_label]
    };

    let mut footer_spans: Vec<Span> = Vec::new();
    let mut action_x = content_x;
    for (index, label) in labels.iter().enumerate() {
        let is_cancel = confirm_always && index == 1;
        let is_reject = !confirm_always && index == 2;
        let is_selected = index == selected;
        let bg = if is_selected {
            if is_reject || is_cancel {
                theme.err()
            } else {
                theme.brand()
            }
        } else {
            theme.raised()
        };
        let fg = if is_selected {
            contrast_fg(bg)
        } else {
            theme.fg()
        };
        if index > 0 {
            footer_spans.push(Span::styled("  ", Style::default().bg(theme.raised())));
            action_x = action_x.saturating_add(2);
        }
        let text = format!(" {} ", label);
        let width = text.width().min(u16::MAX as usize) as u16;
        hit_map.push_permission_action(PermissionActionHit {
            action_index: index,
            rect: Rect::new(action_x, footer_y, width, PERMISSION_FOOTER_HEIGHT),
        });
        footer_spans.push(Span::styled(
            text,
            Style::default().bg(bg).fg(fg).add_modifier(Modifier::BOLD),
        ));
        action_x = action_x.saturating_add(width);
    }
    let hints: &[FooterHint] = if confirm_always {
        &[
            FooterHint::navigation(keyvocab::ARROWS_LR, "select"),
            FooterHint::primary(keyvocab::ENTER, "confirm"),
            FooterHint::always(keyvocab::ESC, "back"),
        ]
    } else if max_scroll > 0 {
        &[
            FooterHint::navigation(keyvocab::ARROWS_UD, "scroll"),
            FooterHint::navigation(keyvocab::ARROWS_LR, "select"),
            FooterHint::primary(keyvocab::ENTER, "confirm"),
            FooterHint::always(keyvocab::ESC, "reject"),
        ]
    } else {
        &[
            FooterHint::navigation(keyvocab::ARROWS_LR, "select"),
            FooterHint::primary(keyvocab::ENTER, "confirm"),
            FooterHint::always(keyvocab::ESC, "reject"),
        ]
    };
    let footer_width = content_w as usize;
    let used: usize = footer_spans.iter().map(|s| s.content.width()).sum();
    let hint = modal_footer_text(hints, footer_width.saturating_sub(used));
    let hint_width = hint.width();
    if used + hint_width <= footer_width {
        footer_spans.push(Span::styled(
            " ".repeat(footer_width - used - hint_width),
            Style::default().bg(theme.raised()),
        ));
        footer_spans.push(Span::styled(
            hint,
            Style::default().bg(theme.raised()).fg(theme.muted()),
        ));
    } else if used < footer_width {
        footer_spans.push(Span::styled(
            " ".repeat(footer_width - used),
            Style::default().bg(theme.raised()),
        ));
    }

    frame.render_widget(
        Paragraph::new(Line::from(footer_spans)),
        Rect::new(content_x, footer_y, content_w, PERMISSION_FOOTER_HEIGHT),
    );
    max_scroll
}

/// Inline input-injection panel (L3.5 β): rendered over the composer rect when
/// an interactive `bash` command needs operator input. A one-line prompt
/// (the command + what to enter) above an input line that mirrors the
/// composer. When `secret` is set the typed text is masked as `•` so a
/// password/passphrase isn't shown in the clear. The panel is a left-bar
/// panel (`panel_block`) so it reads as the same surface language as the
/// permission sheet. Returns the rect it drew into.
pub fn draw_input_injection(
    frame: &mut Frame,
    request: &neenee_contracts::InputRequest,
    input: &str,
    _cursor: usize,
    input_rect: Rect,
    theme: &Theme,
) -> Rect {
    use neenee_tui_engine::Layout;
    // Split the composer rect into a prompt row + the input row(s).
    let chunks = Layout::default()
        .direction(neenee_tui_engine::Direction::Vertical)
        .constraints([
            neenee_tui_engine::Constraint::Length(1),
            neenee_tui_engine::Constraint::Min(0),
        ])
        .split(input_rect);

    let prompt_rect = chunks[0];
    let entry_rect = chunks[1];

    // Prompt line: the command for context, then what to enter.
    let secret_label = if request.secret {
        " (input hidden)"
    } else {
        ""
    };
    let prompt_text = format!("{}  —  {}{}", request.command, request.prompt, secret_label);
    let prompt_line = Line::from(vec![
        Span::styled("┃ ", Style::default().fg(theme.warn())),
        Span::styled(
            request.command.clone(),
            Style::default()
                .fg(theme.warn())
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("  —  {}{}", request.prompt, secret_label),
            Style::default().fg(theme.muted()),
        ),
    ]);
    let _ = prompt_text;
    frame.render_widget(
        RtBlock::default().style(Style::default().bg(theme.user_surface())),
        prompt_rect,
    );
    frame.render_widget(Paragraph::new(prompt_line), prompt_rect);

    // Entry line: mask the typed input when secret, else show it verbatim.
    let display: String = if request.secret {
        "•".repeat(input.chars().count())
    } else {
        input.to_string()
    };
    let entry_prefix = "> ";
    let entry_line = Line::from(vec![
        Span::styled(
            entry_prefix,
            Style::default()
                .fg(theme.brand())
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(display, Style::default().fg(theme.fg())),
        Span::styled(
            "  Enter=submit  Esc=skip (runs non-interactively)",
            Style::default().fg(theme.dim()),
        ),
    ]);
    frame.render_widget(
        RtBlock::default().style(Style::default().bg(theme.input_surface())),
        entry_rect,
    );
    frame.render_widget(Paragraph::new(entry_line), entry_rect);

    input_rect
}

#[cfg(test)]
mod tests {
    use super::*;
    use neenee_contracts::{UserQuestion, UserQuestionOption};

    #[test]
    fn question_modal_records_option_hit_boxes() {
        let request = UserQuestionRequest {
            id: "q".into(),
            questions: vec![UserQuestion {
                header: None,
                question: "Pick one".into(),
                options: vec![
                    UserQuestionOption {
                        label: "A".into(),
                        description: None,
                    },
                    UserQuestionOption {
                        label: "B".into(),
                        description: Some("Second option".into()),
                    },
                ],
                multi_select: false,
            }],
        };
        let mut terminal = neenee_tui_engine::TestTerminal::new(80, 24);
        let mut hit_map = ModalHitMap::new();
        terminal.draw(|frame| {
            let mut scroll = 0;
            draw_question_modal(
                frame,
                &mut hit_map,
                &request,
                0,
                &[vec![0]],
                &[String::new()],
                0,
                &mut scroll,
                true,
                &Theme::default(),
            );
        });

        assert!(find_question_hit(&hit_map, 80, 24, 0));
        assert!(find_question_hit(&hit_map, 80, 24, 1));
        assert!(find_question_hit(&hit_map, 80, 24, 2));
    }

    #[test]
    fn permission_sheet_records_footer_action_hit_boxes() {
        let request = PermissionRequest {
            id: "p".into(),
            tool: "bash".into(),
            label: "bash".into(),
            description: "Run a command".into(),
            arguments: r#"{"command":"cargo test"}"#.into(),
            scope: "*".into(),
            elevation: false,
            one_off: false,
        };
        let mut terminal = neenee_tui_engine::TestTerminal::new(80, 24);
        let mut hit_map = ModalHitMap::new();
        terminal.draw(|frame| {
            let rect = Rect::new(0, 16, 80, 8);
            let _ = draw_permission_sheet(
                frame,
                &mut hit_map,
                &request,
                0,
                false,
                false,
                0,
                rect,
                &Theme::default(),
                &crate::model::selection::SelectionState::None,
                &mut crate::model::layout::LayoutMap::new(),
            );
        });

        for action_index in 0..4 {
            assert!(
                find_permission_hit(&hit_map, 80, 24, action_index),
                "missing permission action {action_index}"
            );
        }
    }

    fn find_question_hit(map: &ModalHitMap, width: u16, height: u16, option_index: usize) -> bool {
        (0..height).any(|y| {
            (0..width).any(|x| {
                map.question_option_at(x, y)
                    .is_some_and(|hit| hit.option_index == option_index)
            })
        })
    }

    fn find_permission_hit(
        map: &ModalHitMap,
        width: u16,
        height: u16,
        action_index: usize,
    ) -> bool {
        (0..height).any(|y| {
            (0..width).any(|x| {
                map.permission_action_at(x, y)
                    .is_some_and(|hit| hit.action_index == action_index)
            })
        })
    }
}
