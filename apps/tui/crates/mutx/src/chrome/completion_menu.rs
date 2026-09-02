//! The floating completion popup menu anchored above the composer input.

use mutx_engine::{Block as RtBlock, Clear, Frame, Line, Modifier, Paragraph, Rect, Span, Style};
use unicode_width::UnicodeWidthStr;

use crate::model::layout::LayoutMap;
use crate::primitives::{contrast_fg, viewport_rect};
use crate::view::Theme;

/// Draw a completion menu anchored above the input box.
pub fn draw_completion_menu(
    frame: &mut Frame,
    _layout_map: &mut LayoutMap,
    hit_map: Option<&mut crate::model::layout::ModalHitMap>,
    completions: &[crate::completion::Completion],
    selected_idx: Option<usize>,
    anchor: Rect,
    anchor_x: u16,
    theme: &Theme,
) {
    if completions.is_empty() {
        return;
    }

    const MAX_VISIBLE: usize = 6;

    let total = completions.len();
    let scroll_offset = match selected_idx {
        Some(sel) if sel >= MAX_VISIBLE && total > MAX_VISIBLE => {
            (sel - (MAX_VISIBLE - 1)).min(total - MAX_VISIBLE)
        }
        _ => 0,
    };
    let window_end = (scroll_offset + MAX_VISIBLE).min(total);
    let visible_rows = &completions[scroll_offset..window_end];
    let menu_height = visible_rows.len() as u16;

    let viewport = viewport_rect(frame);

    let active_doc = selected_idx
        .and_then(|idx| completions.get(idx))
        .and_then(|c| c.doc.as_ref());

    let max_cmd = completions
        .iter()
        .map(|c| match &c.kind {
            crate::completion::CompletionItemKind::IntentSuggestion { .. } => c.label.width() + 2,
            crate::completion::CompletionItemKind::SlashAlias => c.label.width() + 4,
            _ if c.alias_of.is_some() => c.label.width() + 4,
            _ => c.label.width(),
        })
        .max()
        .unwrap_or(0);

    let max_menu_width = ((viewport.width as usize) * 3 / 5).max(24);
    let content_width = (max_cmd + 2).max(18);
    let menu_width = (content_width
        .min(max_menu_width)
        .min(viewport.width as usize)) as u16;

    let mut y = anchor.y.saturating_sub(menu_height);
    if y == 0 && anchor.y < menu_height {
        y = 0;
    }
    let x = anchor_x
        .min(viewport.right().saturating_sub(menu_width))
        .max(viewport.x);

    let menu_area = Rect::new(x, y, menu_width, menu_height);
    frame.render_widget(Clear, menu_area);

    if let Some(hm) = hit_map {
        hm.set_completion_menu_rect(menu_area);
        for row in 0..visible_rows.len() {
            let global_idx = row + scroll_offset;
            let row_rect = Rect::new(menu_area.x, menu_area.y + row as u16, menu_area.width, 1);
            hm.push_completion_item(global_idx, row_rect);
        }
    }

    let block = RtBlock::default().style(Style::default().bg(theme.body()));
    let menu_w = menu_area.width as usize;

    let lines: Vec<Line> = visible_rows
        .iter()
        .enumerate()
        .map(|(row, c)| {
            let global_idx = row + scroll_offset;
            let is_selected = Some(global_idx) == selected_idx;
            let body_bg = theme.body();
            let row_bg = if is_selected { theme.brand() } else { body_bg };
            let is_alias = matches!(c.kind, crate::completion::CompletionItemKind::SlashAlias)
                || c.alias_of.is_some();
            let cmd_style = if is_selected {
                Style::default()
                    .bg(row_bg)
                    .fg(contrast_fg(theme.brand()))
                    .add_modifier(Modifier::BOLD)
            } else if matches!(
                c.kind,
                crate::completion::CompletionItemKind::IntentSuggestion { .. }
            ) {
                Style::default()
                    .bg(row_bg)
                    .fg(theme.info())
                    .add_modifier(Modifier::BOLD)
            } else if is_alias {
                Style::default().bg(row_bg).fg(theme.fg())
            } else {
                Style::default()
                    .bg(row_bg)
                    .fg(theme.fg())
                    .add_modifier(Modifier::BOLD)
            };

            let (primary, secondary) = match &c.kind {
                crate::completion::CompletionItemKind::IntentSuggestion { .. } => {
                    (format!("➜ {}", c.label), String::new())
                }
                crate::completion::CompletionItemKind::SlashAlias => {
                    (format!("{} [*]", c.label), String::new())
                }
                _ if is_alias => (format!("{} [*]", c.label), String::new()),
                _ => (c.label.clone(), String::new()),
            };
            let secondary_style = if is_selected {
                Style::default().bg(row_bg).fg(contrast_fg(theme.brand()))
            } else {
                Style::default().bg(row_bg).fg(theme.muted())
            };

            let used = primary.width() + secondary.width();
            let pad = menu_w.saturating_sub(used);
            let mut spans = vec![Span::styled(primary, cmd_style)];
            if !secondary.is_empty() {
                spans.push(Span::styled(secondary, secondary_style));
            }
            if pad > 0 {
                spans.push(Span::styled(" ".repeat(pad), Style::default().bg(row_bg)));
            }
            Line::from(spans)
        })
        .collect();

    frame.render_widget(Paragraph::new(lines).block(block), menu_area);

    if let Some(doc) = active_doc {
        let space_on_right =
            (viewport.right() as usize).saturating_sub(menu_area.right() as usize + 1);
        let space_on_left = (menu_area.x as usize).saturating_sub(viewport.x as usize + 1);

        let (doc_x, doc_width) = if space_on_right >= 26 {
            let w = space_on_right.min(56) as u16;
            (menu_area.right() + 1, w)
        } else if space_on_left >= 26 {
            let w = space_on_left.min(56) as u16;
            (menu_area.x.saturating_sub(w + 1), w)
        } else {
            (0, 0)
        };

        if doc_width >= 20 {
            let doc_bg = theme.panel();
            let max_text_w = (doc_width as usize).saturating_sub(2).max(10);
            let mut insp_lines = Vec::new();

            let cat_label = doc.category.as_deref().unwrap_or("Command");
            let alias_row = selected_idx.and_then(|idx| completions.get(idx));
            let is_alias = alias_row
                .map(|c| {
                    matches!(c.kind, crate::completion::CompletionItemKind::SlashAlias)
                        || c.alias_of.is_some()
                })
                .unwrap_or(false);

            let header_title = if is_alias {
                let alias_label = alias_row.map(|c| c.label.as_str()).unwrap_or_default();
                let target = alias_row
                    .and_then(|c| c.alias_of.as_deref())
                    .unwrap_or(&doc.name);
                format!("{alias_label} -> {target}")
            } else {
                doc.name.clone()
            };

            let header_spans = vec![
                Span::styled(" ", Style::default().bg(doc_bg)),
                Span::styled(
                    header_title,
                    Style::default()
                        .bg(doc_bg)
                        .fg(theme.info())
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("  [{cat_label}]"),
                    Style::default()
                        .bg(doc_bg)
                        .fg(theme.muted())
                        .add_modifier(Modifier::DIM),
                ),
            ];
            insp_lines.push(Line::from(header_spans));

            if !doc.summary.is_empty() {
                for wl in crate::text_layout::wrap_text(&doc.summary, max_text_w) {
                    insp_lines.push(Line::from(vec![
                        Span::styled(" ", Style::default().bg(doc_bg)),
                        Span::styled(
                            wl.text.to_string(),
                            Style::default()
                                .bg(doc_bg)
                                .fg(theme.fg())
                                .add_modifier(Modifier::BOLD),
                        ),
                    ]));
                }
            }

            if !doc.usage.is_empty() {
                let usage_str = format!("Usage: {}", doc.usage.join("  |  "));
                for wl in crate::text_layout::wrap_text(&usage_str, max_text_w) {
                    insp_lines.push(Line::from(vec![
                        Span::styled(" ", Style::default().bg(doc_bg)),
                        Span::styled(
                            wl.text.to_string(),
                            Style::default()
                                .bg(doc_bg)
                                .fg(theme.brand())
                                .add_modifier(Modifier::DIM),
                        ),
                    ]));
                }
            }

            for (sub_name, sub_summary) in &doc.subcommands {
                let sub_str = format!("  {sub_name} — {sub_summary}");
                for wl in crate::text_layout::wrap_text(&sub_str, max_text_w) {
                    insp_lines.push(Line::from(vec![
                        Span::styled(" ", Style::default().bg(doc_bg)),
                        Span::styled(
                            wl.text.to_string(),
                            Style::default().bg(doc_bg).fg(theme.muted()),
                        ),
                    ]));
                }
            }

            let max_flyout_h = (viewport.height as usize)
                .saturating_sub(anchor.y as usize)
                .clamp(12, 16) as u16;
            let doc_height = (insp_lines.len() as u16).max(menu_height).min(max_flyout_h);
            let mut doc_y = anchor.y.saturating_sub(doc_height);
            if doc_y == 0 && anchor.y < doc_height {
                doc_y = 0;
            }

            let doc_area = Rect::new(doc_x, doc_y, doc_width, doc_height);
            frame.render_widget(Clear, doc_area);

            let doc_w = doc_width as usize;
            let padded_doc_lines: Vec<Line> = (0..doc_height as usize)
                .map(|idx| {
                    if let Some(mut line) = insp_lines.get(idx).cloned() {
                        let cur_w = line.width();
                        if cur_w < doc_w {
                            line.spans.push(Span::styled(
                                " ".repeat(doc_w - cur_w),
                                Style::default().bg(doc_bg),
                            ));
                        }
                        line
                    } else {
                        Line::from(vec![Span::styled(
                            " ".repeat(doc_w),
                            Style::default().bg(doc_bg),
                        )])
                    }
                })
                .collect();

            let doc_block = RtBlock::default().style(Style::default().bg(doc_bg));
            frame.render_widget(Paragraph::new(padded_doc_lines).block(doc_block), doc_area);
        }
    }
}
