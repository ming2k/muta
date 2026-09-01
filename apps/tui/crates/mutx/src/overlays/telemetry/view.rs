//! Telemetry modal view rendering (Overview and Activity tabs, turn lists, attempt waterfall).

use muta_contracts::TokenSourceReport;
use mutx_engine::{Frame, Line, Modifier, Rect, Span, Style};

use super::super::common::placeholder;
use super::model::*;
use crate::components::selectable_body::{SelectableRow, render_selectable_body};
use crate::design::MODAL_INNER_H_PADDING;
use crate::modal::TelemetryTab;
use crate::model::layout::LayoutMap;
use crate::model::selection::SelectionState;
use crate::primitives::{
    BodyRenderOptions, ContentModalSpec, FooterHint, HeaderPart, breadcrumb_parts,
    content_modal_area, content_modal_probe, hierarchical_breadcrumb, keyvocab, modal_chrome_rows,
    modal_frame, modal_header_parts, render_body, render_modal_footer,
};
use crate::view::Theme;

#[allow(clippy::too_many_arguments)]
pub fn draw_telemetry_modal(
    frame: &mut Frame,
    report: &TokenSourceReport,
    context: ContextUsageView,
    tab: TelemetryTab,
    selected: usize,
    detail: bool,
    turn: Option<(u32, u32)>,
    turn_cursor: usize,
    loading: bool,
    scroll: &mut usize,
    theme: &Theme,
    selection: &SelectionState,
    layout_map: &mut LayoutMap,
) -> Rect {
    let geometry = ContentModalSpec::TELEMETRY;
    let probe = content_modal_probe(frame, geometry);
    let body_width = (probe.width as usize)
        .saturating_sub(2 * MODAL_INNER_H_PADDING as usize)
        .max(1);
    let header_width = (probe.width as usize)
        .saturating_sub(2 * MODAL_INNER_H_PADDING as usize)
        .max(1);

    if loading {
        let area = content_modal_area(frame, geometry, 7);
        let modal = modal_frame(frame, area, theme.panel(), true, true);
        modal_header_parts(
            frame,
            modal.header,
            &[HeaderPart::title("Session Telemetry")],
            theme,
        );
        let body = vec![placeholder(
            "Loading session telemetry from daemon…",
            true,
            theme.muted(),
        )];
        render_body(
            frame,
            modal.body,
            body,
            scroll,
            BodyRenderOptions::follow(None),
            theme,
        );
        if let Some(footer_area) = modal.footer {
            render_modal_footer(
                frame,
                footer_area,
                &[FooterHint::key_always(crate::keymap::Key::ESC, "close")],
                theme,
            );
        }
        return area;
    }

    let rounds = extract_telemetry_rounds(report);
    let round_num = rounds.get(selected).map_or(0, |r| r.round_number);
    let round_child = if detail || turn.is_some() {
        if let Some((target_round, _)) = turn {
            format!("Round #{target_round}")
        } else {
            format!("Round #{round_num} Turns")
        }
    } else {
        String::new()
    };
    let turn_child = if let Some((_, target_attempt)) = turn {
        format!("Attempt #{target_attempt}")
    } else {
        String::new()
    };

    if let Some((target_round, target_attempt)) = turn {
        // L3: Attempt Inspector
        let levels = [
            "Session Telemetry",
            round_child.as_str(),
            turn_child.as_str(),
        ];
        let header = hierarchical_breadcrumb(&levels, header_width);
        let body = build_attempt_inspector_body(
            &rounds,
            target_round,
            target_attempt,
            context,
            body_width,
            theme,
        );
        let footer = [
            FooterHint::always(keyvocab::ARROWS_UD, "scroll"),
            FooterHint::key_always(crate::keymap::Key::ESC, "turns"),
        ];

        let desired = body.len() as u16 + modal_chrome_rows(geometry.modal_spec());
        let area = content_modal_area(frame, geometry, desired);
        let modal = modal_frame(frame, area, theme.panel(), true, true);
        modal_header_parts(frame, modal.header, &header, theme);

        let rows: Vec<SelectableRow> = body.into_iter().map(SelectableRow::from_line).collect();
        render_selectable_body(
            frame, modal.body, &rows, scroll, None, theme, selection, layout_map,
        );
        if let Some(footer_area) = modal.footer {
            render_modal_footer(frame, footer_area, &footer, theme);
        }
        area
    } else if detail {
        // L2: Turn List with Sticky Header
        let header = breadcrumb_parts("Session Telemetry", &round_child).to_vec();
        let (table_header, rows, follow) =
            build_turns_table(&rounds, selected, turn_cursor, body_width, theme);
        let footer = [
            FooterHint::always(keyvocab::ARROWS_UD, "select"),
            FooterHint::key_always(crate::keymap::Key::ENTER, "inspect"),
            FooterHint::key_always(crate::keymap::Key::ESC, "rounds"),
        ];

        let desired = (rows.len() + 1) as u16 + modal_chrome_rows(geometry.modal_spec());
        let area = content_modal_area(frame, geometry, desired);
        let modal = modal_frame(frame, area, theme.panel(), true, true);
        modal_header_parts(frame, modal.header, &header, theme);

        let header_h = 1.min(modal.body.height);
        let header_rect = Rect {
            x: modal.body.x,
            y: modal.body.y,
            width: modal.body.width,
            height: header_h,
        };
        let mut header_scroll = 0;
        render_body(
            frame,
            header_rect,
            table_header,
            &mut header_scroll,
            BodyRenderOptions::follow(None),
            theme,
        );

        let table_rect = Rect {
            x: modal.body.x,
            y: modal.body.y.saturating_add(header_h),
            width: modal.body.width,
            height: modal.body.height.saturating_sub(header_h),
        };
        render_body(
            frame,
            table_rect,
            rows,
            scroll,
            BodyRenderOptions::follow(follow),
            theme,
        );

        if let Some(footer_area) = modal.footer {
            render_modal_footer(frame, footer_area, &footer, theme);
        }
        area
    } else {
        // L1: Top Level Tabs (Overview vs Activity)
        let header = vec![HeaderPart::title("Session Telemetry")];

        match tab {
            TelemetryTab::Overview => {
                let tab_strip = vec![tab_strip_line(tab, rounds.len(), theme), Line::from("")];
                let overview = build_overview_body(report, &rounds, context, body_width, theme);
                let body_lines: Vec<Line<'static>> =
                    tab_strip.into_iter().chain(overview).collect();
                let footer = [
                    FooterHint::key_always(crate::keymap::Key::TAB, "2 Activity"),
                    FooterHint::always(keyvocab::ARROWS_UD, "scroll"),
                    FooterHint::key_always(crate::keymap::Key::ENTER, "activity"),
                    FooterHint::key_always(crate::keymap::Key::ESC, "close"),
                ];

                let desired = body_lines.len() as u16 + modal_chrome_rows(geometry.modal_spec());
                let area = content_modal_area(frame, geometry, desired);
                let modal = modal_frame(frame, area, theme.panel(), true, true);
                modal_header_parts(frame, modal.header, &header, theme);

                let rows: Vec<SelectableRow> = body_lines
                    .into_iter()
                    .map(SelectableRow::from_line)
                    .collect();
                render_selectable_body(
                    frame, modal.body, &rows, scroll, None, theme, selection, layout_map,
                );

                if let Some(footer_area) = modal.footer {
                    render_modal_footer(frame, footer_area, &footer, theme);
                }
                area
            }
            TelemetryTab::Activity => {
                let tab_strip = vec![tab_strip_line(tab, rounds.len(), theme), Line::from("")];
                let (table_header, rows, follow) =
                    build_rounds_table(&rounds, selected, body_width, theme);
                let footer = [
                    FooterHint::key_always(crate::keymap::Key::TAB, "1 Overview"),
                    FooterHint::always(keyvocab::ARROWS_UD, "select"),
                    FooterHint::key_always(crate::keymap::Key::ENTER, "turns"),
                    FooterHint::key_always(crate::keymap::Key::ESC, "close"),
                ];

                let desired = (rows.len() + 3) as u16 + modal_chrome_rows(geometry.modal_spec());
                let area = content_modal_area(frame, geometry, desired);
                let modal = modal_frame(frame, area, theme.panel(), true, true);
                modal_header_parts(frame, modal.header, &header, theme);

                // 1. Tab strip (Fixed at top)
                let tab_h = 2.min(modal.body.height);
                let tab_rect = Rect {
                    x: modal.body.x,
                    y: modal.body.y,
                    width: modal.body.width,
                    height: tab_h,
                };
                let mut tab_scroll = 0;
                render_body(
                    frame,
                    tab_rect,
                    tab_strip,
                    &mut tab_scroll,
                    BodyRenderOptions::follow(None),
                    theme,
                );

                // 2. Sticky table header (Fixed right below tab strip)
                let header_h = 1.min(modal.body.height.saturating_sub(tab_h));
                let header_rect = Rect {
                    x: modal.body.x,
                    y: modal.body.y.saturating_add(tab_h),
                    width: modal.body.width,
                    height: header_h,
                };
                let mut header_scroll = 0;
                render_body(
                    frame,
                    header_rect,
                    table_header,
                    &mut header_scroll,
                    BodyRenderOptions::follow(None),
                    theme,
                );

                // 3. Scrollable table body
                let fixed_h = tab_h.saturating_add(header_h);
                let table_rect = Rect {
                    x: modal.body.x,
                    y: modal.body.y.saturating_add(fixed_h),
                    width: modal.body.width,
                    height: modal.body.height.saturating_sub(fixed_h),
                };
                render_body(
                    frame,
                    table_rect,
                    rows,
                    scroll,
                    BodyRenderOptions::follow(follow),
                    theme,
                );

                if let Some(footer_area) = modal.footer {
                    render_modal_footer(frame, footer_area, &footer, theme);
                }
                area
            }
        }
    }
}

pub(crate) fn tab_strip_line(
    active_tab: TelemetryTab,
    rounds_count: usize,
    theme: &Theme,
) -> Line<'static> {
    let (ov_style, act_style) = match active_tab {
        TelemetryTab::Overview => (
            Style::default()
                .fg(theme.brand())
                .add_modifier(Modifier::BOLD),
            Style::default().fg(theme.muted()),
        ),
        TelemetryTab::Activity => (
            Style::default().fg(theme.muted()),
            Style::default()
                .fg(theme.brand())
                .add_modifier(Modifier::BOLD),
        ),
    };
    let rounds_suffix = if rounds_count > 0 {
        format!(" ({rounds_count})")
    } else {
        String::new()
    };
    Line::from(vec![
        Span::styled("  ", Style::default()),
        if active_tab == TelemetryTab::Overview {
            Span::styled("[ 1 Overview ]", ov_style)
        } else {
            Span::styled("  1 Overview  ", ov_style)
        },
        Span::styled("    ", Style::default()),
        if active_tab == TelemetryTab::Activity {
            Span::styled(format!("[ 2 Activity{rounds_suffix} ]"), act_style)
        } else {
            Span::styled(format!("  2 Activity{rounds_suffix}  "), act_style)
        },
    ])
}

// ─────────────────────────────────────────────────────────────────────────────
// Overview Tab Builder
// ─────────────────────────────────────────────────────────────────────────────

pub(crate) fn build_overview_body(
    report: &TokenSourceReport,
    rounds: &[TelemetryRound],
    context: ContextUsageView,
    _width: usize,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();

    // ── 1. Context Window ──
    lines.push(overview_section_header("CONTEXT WINDOW", theme));

    let window_max = context.window_tokens.unwrap_or(0);
    let used = context.snapshot.map(|s| s.tokens).unwrap_or(0);
    let ratio = if window_max == 0 {
        0.0
    } else {
        ((used as f64) / (window_max as f64)).clamp(0.0, 1.0)
    };

    let used_text = if window_max > 0 {
        format!("{} tokens ({:.1}%)", used, ratio * 100.0)
    } else {
        format!("{} tokens", used)
    };

    lines.push(kv_overview_line(
        "Used Tokens",
        &used_text,
        Style::default().fg(theme.fg()),
        theme,
    ));
    if window_max > 0 {
        lines.push(kv_overview_line(
            "Capacity",
            &format!("{} tokens", window_max),
            Style::default().fg(theme.muted()),
            theme,
        ));
    }
    if context.draft_tokens > 0 {
        lines.push(kv_overview_line(
            "Draft Input",
            &format!("~{} tokens", context.draft_tokens),
            Style::default().fg(theme.muted()),
            theme,
        ));
    }

    lines.push(Line::from(""));

    // ── 2. Session Token Totals ──
    lines.push(overview_section_header("SESSION TOKEN TOTALS", theme));

    let total_prompt = report.grand_total.prompt_tokens as u64;
    let total_completion = report.grand_total.completion_tokens as u64;
    let total_cache_read = report.grand_total.cache_read_tokens as u64;
    let total_cache_write = report.grand_total.cache_write_tokens as u64;
    let grand_total = total_prompt + total_completion;

    lines.push(kv_overview_line(
        "Grand Total",
        &format!("{} ({})", fmt_tokens(grand_total), grand_total),
        Style::default()
            .fg(theme.brand())
            .add_modifier(Modifier::BOLD),
        theme,
    ));
    lines.push(kv_overview_line(
        "Input (Prompt)",
        &format!("{} tokens", total_prompt),
        Style::default().fg(theme.fg()),
        theme,
    ));
    lines.push(kv_overview_line(
        "Output (Completion)",
        &format!("{} tokens", total_completion),
        Style::default().fg(theme.fg()),
        theme,
    ));

    let hit_rate = if total_prompt > 0 {
        (total_cache_read as f64 / total_prompt as f64) * 100.0
    } else {
        0.0
    };
    lines.push(kv_overview_line(
        "Cache Read",
        &format!("{} tokens ({:.1}% hit rate)", total_cache_read, hit_rate),
        Style::default().fg(if hit_rate > 0.0 {
            theme.ok()
        } else {
            theme.muted()
        }),
        theme,
    ));
    if total_cache_write > 0 {
        lines.push(kv_overview_line(
            "Cache Written",
            &format!("{} tokens", total_cache_write),
            Style::default().fg(theme.muted()),
            theme,
        ));
    }

    lines.push(Line::from(""));

    // ── 3. Performance & Activity ──
    lines.push(overview_section_header(
        "STREAM PERFORMANCE & ACTIVITY",
        theme,
    ));

    let mut tps_values: Vec<f64> = Vec::new();
    let mut ttft_values_ms: Vec<f64> = Vec::new();
    let mut total_e2e_ms: u64 = 0;
    let mut total_turns: usize = 0;

    for r in rounds {
        total_turns += r.turns_count;
        total_e2e_ms += r.e2e_duration_ms;
        for att in &r.attempts {
            if let Some(tps) = att.preferred_tps() {
                tps_values.push(tps);
            }
            if let Some(perf) = &att.performance
                && let Some(ttft_us) = perf.ttft_us
            {
                ttft_values_ms.push(ttft_us as f64 / 1000.0);
            }
        }
    }

    if !tps_values.is_empty() {
        let avg_tps = tps_values.iter().sum::<f64>() / (tps_values.len() as f64);
        lines.push(kv_overview_line(
            "Avg Stream Rate",
            &format!("{:.1} tok/s", avg_tps),
            Style::default().fg(theme.fg()),
            theme,
        ));
    } else {
        lines.push(kv_overview_line(
            "Avg Stream Rate",
            "–",
            Style::default().fg(theme.muted()),
            theme,
        ));
    }

    if !ttft_values_ms.is_empty() {
        let avg_ttft = ttft_values_ms.iter().sum::<f64>() / (ttft_values_ms.len() as f64);
        lines.push(kv_overview_line(
            "Avg TTFT",
            &format!("{:.0}ms", avg_ttft),
            Style::default().fg(theme.fg()),
            theme,
        ));
    }

    lines.push(kv_overview_line(
        "Total Duration",
        &fmt_duration_ms(total_e2e_ms),
        Style::default().fg(theme.muted()),
        theme,
    ));

    lines.push(kv_overview_line(
        "Activity Count",
        &format!("{} rounds · {} tool turns", rounds.len(), total_turns),
        Style::default().fg(theme.muted()),
        theme,
    ));

    lines
}

fn overview_section_header(title: &str, theme: &Theme) -> Line<'static> {
    Line::from(vec![
        Span::styled("  ", Style::default()),
        Span::styled(
            title.to_string(),
            Style::default()
                .fg(theme.brand())
                .add_modifier(Modifier::BOLD),
        ),
    ])
}

fn kv_overview_line(key: &str, value: &str, val_style: Style, theme: &Theme) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("    {:<22}", key),
            Style::default().fg(theme.muted()),
        ),
        Span::styled(value.to_string(), val_style),
    ])
}

// ─────────────────────────────────────────────────────────────────────────────
// L1: Round Table Builder (Sticky Header + Data Rows)
// ─────────────────────────────────────────────────────────────────────────────

pub(crate) fn build_rounds_table(
    rounds: &[TelemetryRound],
    selected_idx: usize,
    width: usize,
    theme: &Theme,
) -> (Vec<Line<'static>>, Vec<Line<'static>>, Option<usize>) {
    let show_cache = width >= 86;
    let show_turns = width >= 72;

    let col_round = 8;
    let col_tokens = if width >= 76 { 20 } else { 14 };
    let col_cache = if show_cache { 12 } else { 0 };
    let col_tps = 15;
    let col_dur = 11;
    let col_turns = if show_turns { 10 } else { 0 };

    // 1. Fixed Header (1 line)
    let mut header_spans = vec![
        Span::styled(
            format!("  {:<w$}", "Round", w = col_round),
            Style::default()
                .fg(theme.muted())
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            if width >= 76 {
                format!("{:<w$}", "Tokens (In / Out)", w = col_tokens)
            } else {
                format!("{:<w$}", "Tokens", w = col_tokens)
            },
            Style::default()
                .fg(theme.muted())
                .add_modifier(Modifier::BOLD),
        ),
    ];
    if show_cache {
        header_spans.push(Span::styled(
            format!("{:<w$}", "Cache %", w = col_cache),
            Style::default()
                .fg(theme.muted())
                .add_modifier(Modifier::BOLD),
        ));
    }
    header_spans.push(Span::styled(
        format!("{:<w$}", "Stream TPS", w = col_tps),
        Style::default()
            .fg(theme.muted())
            .add_modifier(Modifier::BOLD),
    ));
    header_spans.push(Span::styled(
        format!("{:<w$}", "Duration", w = col_dur),
        Style::default()
            .fg(theme.muted())
            .add_modifier(Modifier::BOLD),
    ));
    if show_turns {
        header_spans.push(Span::styled(
            format!("{:<w$}", "Turns", w = col_turns),
            Style::default()
                .fg(theme.muted())
                .add_modifier(Modifier::BOLD),
        ));
    }

    let header_lines = vec![Line::from(header_spans)];

    if rounds.is_empty() {
        let empty_rows = vec![Line::from(vec![Span::styled(
            "  No settled turns recorded in this session yet.",
            Style::default().fg(theme.muted()),
        )])];
        return (header_lines, empty_rows, None);
    }

    // 2. Data Rows
    let mut rows = Vec::with_capacity(rounds.len());
    for (i, r) in rounds.iter().enumerate() {
        let is_selected = i == selected_idx;
        let row_style = if is_selected {
            Style::default().bg(theme.selected_bg)
        } else {
            Style::default()
        };

        let mut row_spans = vec![
            Span::styled("  ", Style::default()),
            Span::styled(
                format!("{:<w$}", format!("#{}", r.round_number), w = col_round),
                if is_selected {
                    Style::default()
                        .fg(theme.brand())
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme.fg())
                },
            ),
            Span::styled(
                if width >= 76 {
                    format!(
                        "{:<w$}",
                        format!(
                            "{} / {}",
                            fmt_tokens(r.prompt_tokens),
                            fmt_tokens(r.completion_tokens)
                        ),
                        w = col_tokens
                    )
                } else {
                    format!("{:<w$}", fmt_tokens(r.total_tokens), w = col_tokens)
                },
                Style::default().fg(theme.fg()),
            ),
        ];

        if show_cache {
            let cache_pct = r.cache_hit_rate();
            let cache_label = if cache_pct > 0.0 {
                format!("{:.0}%", cache_pct)
            } else {
                "–".to_string()
            };
            row_spans.push(Span::styled(
                format!("{:<w$}", cache_label, w = col_cache),
                if cache_pct > 0.0 {
                    Style::default().fg(theme.ok())
                } else {
                    Style::default().fg(theme.muted())
                },
            ));
        }

        let tps_label = fmt_tps(r.preferred_tps());
        row_spans.push(Span::styled(
            format!("{:<w$}", tps_label, w = col_tps),
            Style::default().fg(theme.fg()),
        ));

        let dur_label = fmt_duration_ms(r.e2e_duration_ms);
        row_spans.push(Span::styled(
            format!("{:<w$}", dur_label, w = col_dur),
            Style::default().fg(theme.muted()),
        ));

        if show_turns {
            let turns_label = if r.turns_count > 1 {
                format!("{} turns", r.turns_count)
            } else {
                "1 turn".to_string()
            };
            row_spans.push(Span::styled(
                turns_label,
                Style::default().fg(theme.muted()),
            ));
        }

        let mut line = Line::from(row_spans);
        if is_selected {
            line = line.style(row_style);
        }
        rows.push(line);
    }

    let follow = if selected_idx < rounds.len() {
        Some(selected_idx)
    } else {
        None
    };

    (header_lines, rows, follow)
}

// ─────────────────────────────────────────────────────────────────────────────
// L2: Turn Table Builder (Sticky Header + Data Rows)
// ─────────────────────────────────────────────────────────────────────────────

pub(crate) fn build_turns_table(
    rounds: &[TelemetryRound],
    selected_round_idx: usize,
    selected_turn_idx: usize,
    width: usize,
    theme: &Theme,
) -> (Vec<Line<'static>>, Vec<Line<'static>>, Option<usize>) {
    let round = rounds.get(selected_round_idx);
    let attempts = round.map_or(&[] as &[TelemetryAttempt], |r| &r.attempts);

    let col_turn = 10;
    let col_tokens = if width >= 76 { 20 } else { 14 };
    let col_ttft = 12;
    let col_tps = 15;
    let col_dur = 11;
    let col_status = 12;

    let header_spans = vec![
        Span::styled(
            format!("  {:<w$}", "Turn", w = col_turn),
            Style::default()
                .fg(theme.muted())
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            if width >= 76 {
                format!("{:<w$}", "Tokens (In / Out)", w = col_tokens)
            } else {
                format!("{:<w$}", "Tokens", w = col_tokens)
            },
            Style::default()
                .fg(theme.muted())
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("{:<w$}", "TTFT", w = col_ttft),
            Style::default()
                .fg(theme.muted())
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("{:<w$}", "Stream TPS", w = col_tps),
            Style::default()
                .fg(theme.muted())
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("{:<w$}", "Duration", w = col_dur),
            Style::default()
                .fg(theme.muted())
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("{:<w$}", "Status", w = col_status),
            Style::default()
                .fg(theme.muted())
                .add_modifier(Modifier::BOLD),
        ),
    ];

    let header_lines = vec![Line::from(header_spans)];

    if attempts.is_empty() {
        let empty_rows = vec![Line::from(vec![Span::styled(
            "  No attempts recorded for this round.",
            Style::default().fg(theme.muted()),
        )])];
        return (header_lines, empty_rows, None);
    }

    let mut rows = Vec::with_capacity(attempts.len());
    for (i, att) in attempts.iter().enumerate() {
        let is_selected = i == selected_turn_idx;
        let row_style = if is_selected {
            Style::default().bg(theme.selected_bg)
        } else {
            Style::default()
        };

        let turn_label = format!("Turn {}.{}", att.round, att.turn);

        let mut row_spans = vec![
            Span::styled("  ", Style::default()),
            Span::styled(
                format!("{:<w$}", turn_label, w = col_turn),
                if is_selected {
                    Style::default()
                        .fg(theme.brand())
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme.fg())
                },
            ),
            Span::styled(
                if width >= 76 {
                    format!(
                        "{:<w$}",
                        format!(
                            "{} / {}",
                            fmt_tokens(att.prompt_tokens),
                            fmt_tokens(att.completion_tokens)
                        ),
                        w = col_tokens
                    )
                } else {
                    format!(
                        "{:<w$}",
                        fmt_tokens(att.prompt_tokens + att.completion_tokens),
                        w = col_tokens
                    )
                },
                Style::default().fg(theme.fg()),
            ),
        ];

        let ttft_label = att
            .performance
            .as_ref()
            .and_then(|p| p.ttft_us)
            .map(|us| format!("{:.0}ms", us as f64 / 1000.0))
            .unwrap_or_else(|| "–".to_string());
        row_spans.push(Span::styled(
            format!("{:<w$}", ttft_label, w = col_ttft),
            Style::default().fg(theme.fg()),
        ));

        let tps_label = fmt_tps(att.preferred_tps());
        row_spans.push(Span::styled(
            format!("{:<w$}", tps_label, w = col_tps),
            Style::default().fg(theme.fg()),
        ));

        let dur_label = fmt_duration_ms(att.e2e_duration_ms);
        row_spans.push(Span::styled(
            format!("{:<w$}", dur_label, w = col_dur),
            Style::default().fg(theme.muted()),
        ));

        let status_lbl = status_label(att.status);
        let status_st = status_style(att.status, theme);
        row_spans.push(Span::styled(status_lbl, status_st));

        let mut line = Line::from(row_spans);
        if is_selected {
            line = line.style(row_style);
        }
        rows.push(line);
    }

    let follow = if selected_turn_idx < attempts.len() {
        Some(selected_turn_idx)
    } else {
        None
    };

    (header_lines, rows, follow)
}

// ─────────────────────────────────────────────────────────────────────────────
// L3: Attempt Inspector Body (Vertical Waterfall)
// ─────────────────────────────────────────────────────────────────────────────

pub(crate) fn build_attempt_inspector_body(
    rounds: &[TelemetryRound],
    target_round: u32,
    target_attempt: u32,
    context: ContextUsageView,
    _width: usize,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let attempt = rounds
        .iter()
        .find(|r| r.round_number == target_round as u64)
        .and_then(|r| r.attempts.iter().find(|a| a.attempt == target_attempt));

    let mut lines = Vec::new();

    if let Some(att) = attempt {
        // Top identity row
        let connection_display = if att.provider.is_empty() {
            "default".to_string()
        } else {
            att.provider.clone()
        };

        lines.push(Line::from(vec![
            Span::styled(" Target:  ", Style::default().fg(theme.text_muted)),
            Span::styled(
                format!("{} @ {}", att.model, connection_display),
                Style::default()
                    .fg(theme.brand())
                    .add_modifier(Modifier::BOLD),
            ),
        ]));
        lines.push(Line::from(vec![
            Span::styled(" Status:  ", Style::default().fg(theme.text_muted)),
            Span::styled(
                format!("{:?}", att.status),
                status_style(att.status, theme).add_modifier(Modifier::BOLD),
            ),
            Span::raw("   "),
            Span::styled("Attempt: ", Style::default().fg(theme.text_muted)),
            Span::styled(
                format!("Turn {}.{} (attempt #{})", att.round, att.turn, att.attempt),
                Style::default().fg(theme.text),
            ),
        ]));
        lines.push(Line::from(""));

        // Context Space Section
        lines.push(overview_section_header("CONTEXT SPACE", theme));

        let cache_pct = if att.prompt_tokens > 0 {
            (att.cache_read_tokens as f64 / att.prompt_tokens as f64) * 100.0
        } else {
            0.0
        };

        let window_max = context.window_tokens.unwrap_or(200_000);
        let ctx_pct = (att.prompt_tokens as f64 / window_max as f64) * 100.0;
        let bar_width = 24;
        let filled = ((ctx_pct / 100.0) * bar_width as f64).round() as usize;
        let filled = filled.min(bar_width);
        let empty = bar_width.saturating_sub(filled);
        let bar = format!("[{}{}]", "█".repeat(filled), "░".repeat(empty));

        lines.push(Line::from(vec![
            Span::styled(
                "  Input Context:     ",
                Style::default().fg(theme.text_muted),
            ),
            Span::styled(
                format!("{:<10}", fmt_tokens(att.prompt_tokens)),
                Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(
                    "{bar} {ctx_pct:.1}% of {} max",
                    fmt_tokens(window_max as u64)
                ),
                Style::default().fg(theme.text_muted),
            ),
        ]));

        lines.push(Line::from(vec![
            Span::styled(
                "   ├─ Cached Read:   ",
                Style::default().fg(theme.text_muted),
            ),
            Span::styled(
                format!("{:<10}", fmt_tokens(att.cache_read_tokens)),
                Style::default().fg(if att.cache_read_tokens > 0 {
                    theme.success
                } else {
                    theme.text
                }),
            ),
            Span::styled(
                format!("({cache_pct:.1}% Cache Hit)"),
                Style::default().fg(if cache_pct > 0.0 {
                    theme.success
                } else {
                    theme.text_muted
                }),
            ),
        ]));

        let fresh_input = att.prompt_tokens.saturating_sub(att.cache_read_tokens);
        lines.push(Line::from(vec![
            Span::styled(
                "   ├─ Fresh Input:   ",
                Style::default().fg(theme.text_muted),
            ),
            Span::styled(
                format!("{:<10}", fmt_tokens(fresh_input)),
                Style::default().fg(theme.text),
            ),
        ]));

        if att.cache_write_tokens > 0 {
            lines.push(Line::from(vec![
                Span::styled(
                    "   ├─ Cache Created: ",
                    Style::default().fg(theme.text_muted),
                ),
                Span::styled(
                    format!("{:<10}", fmt_tokens(att.cache_write_tokens)),
                    Style::default().fg(theme.warning),
                ),
            ]));
        }

        lines.push(Line::from(vec![
            Span::styled(
                "  Output Generated:  ",
                Style::default().fg(theme.text_muted),
            ),
            Span::styled(
                format!("{:<10}", fmt_tokens(att.completion_tokens)),
                Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
            ),
        ]));
        lines.push(Line::from(""));

        // Latency Timeline Waterfall Section
        lines.push(overview_section_header("LATENCY TIMELINE WATERFALL", theme));
        lines.push(Line::from(""));

        let perf = att.performance;

        // Node 0: Request Dispatched
        lines.push(Line::from(vec![
            Span::styled(
                "  ● 0.00s   ",
                Style::default()
                    .fg(theme.brand())
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "Request Dispatched",
                Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
            ),
        ]));
        lines.push(Line::from(vec![Span::styled(
            "  │",
            Style::default().fg(theme.dim()),
        )]));

        // Branch 1: Connect & Handshake
        let ready_us = perf.and_then(|p| p.stream_ready_us);
        let ready_str = ready_us.map_or("–".to_string(), fmt_duration_us);
        lines.push(Line::from(vec![
            Span::styled(
                "  ├─ Connect & Handshake",
                Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(" ({ready_str})"),
                Style::default().fg(theme.brand()),
            ),
        ]));
        lines.push(Line::from(vec![Span::styled(
            "  │    DNS + TLS + Gateway + Send Request Payload",
            Style::default().fg(theme.text_muted),
        )]));
        lines.push(Line::from(vec![Span::styled(
            "  │",
            Style::default().fg(theme.dim()),
        )]));

        // Node 1: Stream Ready
        let ready_node_time = ready_us.map_or("+–".to_string(), |us| {
            format!("+{:.2}s", us as f64 / 1_000_000.0)
        });
        lines.push(Line::from(vec![
            Span::styled(
                format!("  ● {ready_node_time:<8}"),
                Style::default()
                    .fg(theme.brand())
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "Stream Ready",
                Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                " (HTTP 200 Headers received)",
                Style::default().fg(theme.text_muted),
            ),
        ]));
        lines.push(Line::from(vec![Span::styled(
            "  │",
            Style::default().fg(theme.dim()),
        )]));

        // Branch 2: Prefill & Server Queue
        let ttft_us = perf.and_then(|p| p.ttft_us);
        let prefill_us = match (ttft_us, ready_us) {
            (Some(ttft), Some(ready)) => Some(ttft.saturating_sub(ready)),
            _ => None,
        };
        let prefill_str = prefill_us.map_or("–".to_string(), fmt_duration_us);
        lines.push(Line::from(vec![
            Span::styled(
                "  ├─ Prefill & Server Queue",
                Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(" ({prefill_str})"),
                Style::default().fg(theme.warning),
            ),
        ]));
        lines.push(Line::from(vec![Span::styled(
            format!(
                "  │    Prompt Processing ({} cached / {} eval)",
                fmt_tokens(att.cache_read_tokens),
                fmt_tokens(fresh_input)
            ),
            Style::default().fg(theme.text_muted),
        )]));
        lines.push(Line::from(vec![Span::styled(
            "  │",
            Style::default().fg(theme.dim()),
        )]));

        // Node 2: First Token Arrived (TTFT)
        let ttft_node_time = ttft_us.map_or("+–".to_string(), |us| {
            format!("+{:.2}s", us as f64 / 1_000_000.0)
        });
        let ttft_val_str = ttft_us.map_or("–".to_string(), fmt_duration_us);
        lines.push(Line::from(vec![
            Span::styled(
                format!("  ● {ttft_node_time:<8}"),
                Style::default()
                    .fg(theme.success)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "First Token Arrived",
                Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(" (Client TTFT: {ttft_val_str})"),
                Style::default().fg(theme.success),
            ),
        ]));
        lines.push(Line::from(vec![Span::styled(
            "  │",
            Style::default().fg(theme.dim()),
        )]));

        // Branch 3: Stream Decode
        let stream_us = perf.and_then(|p| p.stream_us);
        let stream_str = stream_us.map_or("–".to_string(), fmt_duration_us);
        let stream_tps_str = fmt_tps(att.preferred_tps());
        let streamed_tok_str = if let Some(p) = perf {
            if p.streamed_output_tokens > 0 {
                p.streamed_output_tokens
            } else {
                att.completion_tokens
            }
        } else {
            att.completion_tokens
        };

        lines.push(Line::from(vec![
            Span::styled(
                "  ├─ Stream Decode",
                Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(" ({stream_str})"),
                Style::default().fg(theme.success),
            ),
        ]));
        lines.push(Line::from(vec![Span::styled(
            format!("  │    {streamed_tok_str} tokens generated @ {stream_tps_str}"),
            Style::default().fg(theme.text_muted),
        )]));
        lines.push(Line::from(vec![Span::styled(
            "  │",
            Style::default().fg(theme.dim()),
        )]));

        // Node 3: Last Token Received
        let last_token_us = match (ttft_us, stream_us) {
            (Some(ttft), Some(stream)) => Some(ttft + stream),
            _ => None,
        };
        let last_node_time = last_token_us.map_or("+–".to_string(), |us| {
            format!("+{:.2}s", us as f64 / 1_000_000.0)
        });
        lines.push(Line::from(vec![
            Span::styled(
                format!("  ● {last_node_time:<8}"),
                Style::default()
                    .fg(theme.brand())
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "Last Token Received",
                Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
            ),
        ]));
        lines.push(Line::from(vec![Span::styled(
            "  │",
            Style::default().fg(theme.dim()),
        )]));

        // Branch 4: Tail & Commit
        let tail_us = perf.and_then(|p| p.tail_us);
        let tail_str = tail_us.map_or("–".to_string(), fmt_duration_us);
        lines.push(Line::from(vec![
            Span::styled(
                "  ├─ Tail & Commit",
                Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(" ({tail_str})"),
                Style::default().fg(theme.text_muted),
            ),
        ]));
        lines.push(Line::from(vec![Span::styled(
            "  │    Stream EOF verification + schema parse",
            Style::default().fg(theme.text_muted),
        )]));
        lines.push(Line::from(vec![Span::styled(
            "  │",
            Style::default().fg(theme.dim()),
        )]));

        // Node 4: Final Completed
        let e2e_us = perf
            .and_then(|p| p.e2e_us)
            .unwrap_or(att.e2e_duration_ms * 1_000);
        let e2e_str = fmt_duration_us(e2e_us);
        lines.push(Line::from(vec![
            Span::styled(
                format!("  ■ +{e2e_str}  "),
                Style::default()
                    .fg(theme.brand())
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "Request Completed",
                Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(" (Total E2E: {e2e_str})"),
                Style::default().fg(theme.brand()),
            ),
        ]));
    } else {
        lines.push(Line::from(vec![Span::styled(
            "  Attempt record not found.",
            Style::default().fg(theme.text_muted),
        )]));
    }

    lines
}
