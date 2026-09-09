//! Telemetry modal view rendering (Overview and Activity tabs, turn lists, attempt waterfall).

use muta_contracts::TokenSourceReport;
use mutx_engine::{Frame, Line, Modifier, Rect, Span, Style};

use super::super::common::placeholder;
use super::TelemetryTab;
use super::model::*;
use crate::components::selectable_body::{SelectableRow, render_selectable_body};
use crate::design::MODAL_INNER_H_PADDING;
use crate::model::layout::LayoutMap;
use crate::model::selection::SelectionState;
use crate::primitives::{
    BodyRenderOptions, ContentModalSpec, FooterHint, HeaderPart, breadcrumb_parts,
    content_modal_area, content_modal_probe, hierarchical_breadcrumb, keyvocab, modal_chrome_rows,
    modal_frame, modal_header_parts, render_body, render_modal_footer,
};
use crate::render::Theme;

#[allow(clippy::too_many_arguments)]
pub fn draw_telemetry_modal(
    frame: &mut Frame,
    report: &TokenSourceReport,
    context: ContextUsageProps,
    tab: TelemetryTab,
    selected: usize,
    detail: bool,
    turn: Option<(u32, u32)>,
    turn_cursor: usize,
    submitted_at_ms: Option<u64>,
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
        let modal = modal_frame(frame, area, theme, true, true);
        modal_header_parts(
            frame,
            modal.header,
            &[HeaderPart::title("Session Stats")],
            theme,
        );
        let body = vec![placeholder(
            "Loading session stats from daemon…",
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
        let levels = ["Session Stats", round_child.as_str(), turn_child.as_str()];
        let header = hierarchical_breadcrumb(&levels, header_width);
        let body = build_attempt_inspector_body(
            &rounds,
            target_round,
            target_attempt,
            context,
            body_width,
            submitted_at_ms,
            theme,
        );
        let footer = [
            FooterHint::always(keyvocab::ARROWS_UD, "scroll"),
            FooterHint::key_always(crate::keymap::Key::ESC, "turns"),
        ];

        let desired = body.len() as u16 + modal_chrome_rows(geometry.modal_spec());
        let area = content_modal_area(frame, geometry, desired);
        let modal = modal_frame(frame, area, theme, true, true);
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
        let header = breadcrumb_parts("Session Stats", &round_child).to_vec();
        let (table_header, rows, follow) =
            build_turns_table(&rounds, selected, turn_cursor, body_width, theme);
        let footer = [
            FooterHint::always(keyvocab::ARROWS_UD, "select"),
            FooterHint::key_always(crate::keymap::Key::ENTER, "inspect"),
            FooterHint::key_always(crate::keymap::Key::ESC, "rounds"),
        ];

        let desired = (rows.len() + 1) as u16 + modal_chrome_rows(geometry.modal_spec());
        let area = content_modal_area(frame, geometry, desired);
        let modal = modal_frame(frame, area, theme, true, true);
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
        let header = vec![HeaderPart::title("Session Stats")];

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
                let modal = modal_frame(frame, area, theme, true, true);
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
                let modal = modal_frame(frame, area, theme, true, true);
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

// Overview Tab Builder

pub(crate) fn build_overview_body(
    report: &TokenSourceReport,
    rounds: &[TelemetryRound],
    context: ContextUsageProps,
    _width: usize,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();

    // 1. Context Window
    lines.push(overview_section_header("CONTEXT WINDOW", theme));

    let window_max = context.window_tokens.unwrap_or(0);
    let used = context.snapshot.map(|s| s.tokens).unwrap_or(0);
    let ratio = if window_max == 0 {
        0.0
    } else {
        ((used as f64) / (window_max as f64)).clamp(0.0, 1.0)
    };

    let used_text = if window_max > 0 {
        format!("{} tokens ({:.1}%)", fmt_num(used), ratio * 100.0)
    } else {
        format!("{} tokens", fmt_num(used))
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
            &format!("{} tokens", fmt_num(window_max)),
            Style::default().fg(theme.muted()),
            theme,
        ));
    }
    if context.draft_tokens > 0 {
        lines.push(kv_overview_line(
            "Draft Input",
            &format!("~{} tokens", fmt_num(context.draft_tokens)),
            Style::default().fg(theme.muted()),
            theme,
        ));
    }

    lines.push(Line::from(""));

    // 2. Session Token Totals
    lines.push(overview_section_header("SESSION TOKEN TOTALS", theme));

    let total_prompt = report.grand_total.prompt_tokens as u64;
    let total_completion = report.grand_total.completion_tokens as u64;
    let total_cache_read = report.grand_total.cache_read_tokens as u64;
    let total_cache_write = report.grand_total.cache_write_tokens as u64;
    let grand_total = total_prompt + total_completion;

    lines.push(kv_overview_line(
        "Grand Total",
        &format!("{} ({})", fmt_tokens(grand_total), fmt_num(grand_total)),
        Style::default()
            .fg(theme.brand())
            .add_modifier(Modifier::BOLD),
        theme,
    ));
    lines.push(kv_overview_line(
        "Input (Prompt)",
        &format!("{} tokens", fmt_num(total_prompt)),
        Style::default().fg(theme.fg()),
        theme,
    ));
    lines.push(kv_overview_line(
        "Output (Completion)",
        &format!("{} tokens", fmt_num(total_completion)),
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
        &format!(
            "{} tokens ({:.1}% hit rate)",
            fmt_num(total_cache_read),
            hit_rate
        ),
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
            &format!("{} tokens", fmt_num(total_cache_write)),
            Style::default().fg(theme.muted()),
            theme,
        ));
    }

    lines.push(Line::from(""));

    // 3. Streaming performance
    lines.push(overview_section_header("STREAMING PERFORMANCE", theme));

    // One rate, one aggregation: sum the tokens and the spans, then divide. The
    // reader can reproduce it from the per-turn numbers in the table below.
    let mut tokens: u64 = 0;
    let mut span_us: u64 = 0;
    let mut ttft_ms: Vec<f64> = Vec::new();
    let mut total_e2e_ms: u64 = 0;
    let mut total_turns: usize = 0;

    for r in rounds {
        total_turns += r.turns_count;
        total_e2e_ms += r.e2e_duration_ms;
        for att in &r.attempts {
            if att.stream_tps().is_some()
                && let Some(span) = att.stream_span_us()
            {
                tokens += att.completion_tokens;
                span_us = span_us.saturating_add(span);
            }
            if let Some(perf) = &att.performance
                && let Some(ttft_us) = perf.ttft_us
            {
                ttft_ms.push(ttft_us as f64 / 1000.0);
            }
        }
    }

    let rate = (tokens > 0 && span_us > 0)
        .then(|| tokens as f64 * 1_000_000.0 / span_us as f64)
        .filter(|rate| rate.is_finite() && *rate > 0.0);
    lines.push(kv_overview_line(
        "Streaming Rate",
        &fmt_tps(rate),
        Style::default().fg(theme.fg()),
        theme,
    ));
    lines.push(kv_overview_line(
        "  (tokens / span)",
        &if rate.is_some() {
            format!("{} tok over {}", fmt_num(tokens), fmt_duration_us(span_us))
        } else {
            "–".to_string()
        },
        Style::default().fg(theme.muted()),
        theme,
    ));

    // Latency is reported as a median: with a handful of turns, a mean is
    // decided by whichever request was unluckiest.
    lines.push(kv_overview_line(
        "TTFT (median)",
        &match median(&mut ttft_ms) {
            Some(median) => format!("{median:.0}ms"),
            None => "–".to_string(),
        },
        Style::default().fg(theme.fg()),
        theme,
    ));

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

/// Median of a non-empty slice (in place).
fn median(values: &mut [f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let middle = values.len() / 2;
    if values.len() % 2 == 1 {
        Some(values[middle])
    } else {
        Some((values[middle - 1] + values[middle]) / 2.0)
    }
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

// L1: Round Table Builder (Sticky Header + Data Rows)

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

        let tps_label = fmt_tps(r.stream_tps());
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

// L2: Turn Table Builder (Sticky Header + Data Rows)

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

        let turn_label = if att.attempt > 1 {
            format!("#{}.{}", att.turn, att.attempt)
        } else {
            format!("#{}", att.turn)
        };

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

        let tps_label = fmt_tps(att.stream_tps());
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

// L3: Attempt Inspector Body (Vertical Waterfall)

pub(crate) fn build_attempt_inspector_body(
    rounds: &[TelemetryRound],
    target_round: u32,
    target_attempt: u32,
    context: ContextUsageProps,
    _width: usize,
    submitted_at_ms: Option<u64>,
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
                format!("Turn #{} (attempt #{})", att.turn, att.attempt),
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
                format!("{bar} {ctx_pct:.1}% of {} max", fmt_num(window_max as u64)),
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

        // Latency timeline: Enter → turn end, every stage in order.
        lines.push(Line::from(""));
        lines.push(overview_section_header("LATENCY TIMELINE", theme));
        lines.push(Line::from(vec![Span::styled(
            "  From Enter to the settled turn — one row per stage",
            Style::default().fg(theme.text_muted),
        )]));
        lines.push(Line::from(""));

        let perf = att.performance;

        if let Some(p) = perf {
            let mut wf_spans = Vec::new();
            wf_spans.push(Span::styled("  ", Style::default()));
            let is_reused = p.dns_us.is_none() && p.tcp_us.is_none() && p.tls_us.is_none();
            if is_reused {
                wf_spans.push(Span::styled(
                    "[Warm Pool 0ms]",
                    Style::default().fg(theme.success),
                ));
            } else {
                if let Some(dns) = p.dns_us {
                    wf_spans.push(Span::styled(
                        format!("[DNS {}]", fmt_duration_us(dns)),
                        Style::default().fg(theme.brand()),
                    ));
                    wf_spans.push(Span::styled(" ─> ", Style::default().fg(theme.dim())));
                }
                if let Some(tcp) = p.tcp_us {
                    wf_spans.push(Span::styled(
                        format!("[TCP {}]", fmt_duration_us(tcp)),
                        Style::default().fg(theme.brand()),
                    ));
                    wf_spans.push(Span::styled(" ─> ", Style::default().fg(theme.dim())));
                }
                if let Some(tls) = p.tls_us {
                    wf_spans.push(Span::styled(
                        format!("[TLS {}]", fmt_duration_us(tls)),
                        Style::default().fg(theme.brand()),
                    ));
                }
            }
            if let Some(ttft) = p.ttft_us {
                wf_spans.push(Span::styled(" ─> ", Style::default().fg(theme.dim())));
                wf_spans.push(Span::styled(
                    format!("[TTFT {}]", fmt_duration_us(ttft)),
                    Style::default().fg(theme.warning),
                ));
            }
            if let Some(stream_us) = p.stream_us {
                wf_spans.push(Span::styled(" ─> ", Style::default().fg(theme.dim())));
                let tps_str = p
                    .stream_tps(att.completion_tokens as i64)
                    .map(|rate| format!(" @ {rate:.1} tok/s"))
                    .unwrap_or_default();
                wf_spans.push(Span::styled(
                    format!("[Stream {}{}]", fmt_duration_us(stream_us), tps_str),
                    Style::default().fg(theme.text),
                ));
            }
            lines.push(Line::from(wf_spans));

            let rtt_str = p.rtt_us.map(fmt_duration_us).unwrap_or_else(|| "–".into());
            let socket_line = format!(
                "  Egress: reused={} · RTT={} · retransmits={}",
                if is_reused { "yes" } else { "no" },
                rtt_str,
                p.retransmits
            );
            lines.push(Line::from(vec![Span::styled(
                socket_line,
                Style::default().fg(theme.text_muted),
            )]));
            lines.push(Line::from(""));
        }

        // The TUI knows when Enter was pressed; the ledger knows when the
        // provider was called. Their difference is everything the daemon did
        // before dispatch (queueing, context projection, hooks).
        let pre_dispatch_ms: Option<u64> = match (submitted_at_ms, att.started_at_ms) {
            (Some(submitted), started) if started >= submitted => Some(started - submitted),
            _ => None,
        };
        let base_ms = pre_dispatch_ms.unwrap_or(0) as f64 / 1000.0;

        let node = |lines: &mut Vec<Line<'static>>,
                    at: Option<f64>,
                    glyph: &str,
                    name: &str,
                    detail: String,
                    style: Style| {
            let stamp = at.map_or("        –".to_string(), |at| format!("{at:>8.2}s"));
            lines.push(Line::from(vec![
                Span::styled(
                    format!("  {stamp} {glyph} "),
                    Style::default()
                        .fg(theme.brand())
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    name.to_string(),
                    Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
                ),
                Span::styled(format!("   {detail}"), style),
            ]));
            lines.push(Line::from(vec![Span::styled(
                "  │",
                Style::default().fg(theme.dim()),
            )]));
        };
        let muted = Style::default().fg(theme.text_muted);
        let accent = Style::default().fg(theme.brand());
        let good = Style::default().fg(theme.success);
        let warn = Style::default().fg(theme.warning);

        // Enter (only when the composer timestamp is known).
        if pre_dispatch_ms.is_some() {
            node(
                &mut lines,
                Some(0.0),
                "●",
                "Enter",
                "you submitted the prompt".to_string(),
                muted,
            );
        }
        node(
            &mut lines,
            Some(base_ms),
            "●",
            "Request dispatched",
            if pre_dispatch_ms.is_some() {
                format!("{base_ms:.2}s local: queue, context projection, hooks")
            } else {
                "timeline starts here (composer timestamp unavailable)".to_string()
            },
            muted,
        );

        // Connection: the phases we can measure, or the fact that none happened.
        let is_reused = match perf {
            Some(p) => p.dns_us.is_none() && p.tcp_us.is_none() && p.tls_us.is_none(),
            None => true,
        };
        let (connect_detail, connect_style) = if is_reused {
            (
                "reused warm pool connection (0ms handshake)".to_string(),
                good,
            )
        } else {
            let p = perf.unwrap_or_default();
            let dns = p.dns_us.map(fmt_duration_us).unwrap_or_else(|| "–".into());
            let tcp = p.tcp_us.map(fmt_duration_us).unwrap_or_else(|| "–".into());
            let tls = p.tls_us.map(fmt_duration_us).unwrap_or_else(|| "–".into());
            (
                format!("cold start: DNS {dns} · TCP {tcp} · TLS {tls}"),
                accent,
            )
        };
        node(
            &mut lines,
            perf.and_then(|p| p.stream_ready_us)
                .map(|us| base_ms + us as f64 / 1000.0),
            "●",
            "Connection ready",
            connect_detail,
            connect_style,
        );

        // Request upload: from dispatch to the last byte handed to the kernel.
        let sent_us = perf.and_then(|p| p.request_sent_us);
        node(
            &mut lines,
            sent_us.map(|us| base_ms + us as f64 / 1000.0),
            "●",
            "Request sent",
            sent_us.map_or("not recorded".to_string(), |us| {
                format!("upload complete after {}", fmt_duration_us(us))
            }),
            muted,
        );

        // Response head: the server has answered, body pending.
        let head_us = perf.and_then(|p| p.stream_ready_us);
        node(
            &mut lines,
            head_us.map(|us| base_ms + us as f64 / 1000.0),
            "●",
            "Response headers",
            head_us.map_or("not recorded".to_string(), |us| {
                format!(
                    "server accepted the request · {} from dispatch",
                    fmt_duration_us(us)
                )
            }),
            accent,
        );

        // First origin frame: the model started responding.
        let frame_us = perf.and_then(|p| p.first_frame_us);
        node(
            &mut lines,
            frame_us.map(|us| base_ms + us as f64 / 1000.0),
            "●",
            "Server started",
            frame_us.map_or("not recorded".to_string(), |us| {
                format!(
                    "first frame from the origin · {} from dispatch",
                    fmt_duration_us(us)
                )
            }),
            accent,
        );

        // First token: the new TTFT anchor is the request being sent.
        let ttft_us = perf.and_then(|p| p.ttft_us);
        let ttft_after_sent = match (sent_us, ttft_us) {
            (Some(sent), Some(ttft)) if ttft >= sent => Some(ttft - sent),
            _ => None,
        };
        node(
            &mut lines,
            ttft_us.map(|us| base_ms + us as f64 / 1000.0),
            "●",
            "First token",
            match (ttft_after_sent, ttft_us) {
                (Some(after), Some(total)) => format!(
                    "TTFT {} after the request was sent · {} from dispatch",
                    fmt_duration_us(after),
                    fmt_duration_us(total)
                ),
                (None, Some(total)) => {
                    format!(
                        "{} from dispatch (request-sent anchor missing)",
                        fmt_duration_us(total)
                    )
                }
                _ => "no output observed".to_string(),
            },
            good,
        );

        // Last token: the stream span is the rate's denominator.
        let stream_us = perf.and_then(|p| p.stream_us);
        let last_token_us = match (ttft_us, stream_us) {
            (Some(ttft), Some(span)) => Some(ttft.saturating_add(span)),
            _ => None,
        };
        let rate = att.stream_tps();
        node(
            &mut lines,
            last_token_us.map(|us| base_ms + us as f64 / 1000.0),
            "●",
            "Last token",
            match (stream_us, rate) {
                (Some(span), Some(rate)) => format!(
                    "streamed {} · {} tok @ {}",
                    fmt_duration_us(span),
                    fmt_num(att.completion_tokens),
                    fmt_tps(Some(rate))
                ),
                (Some(span), None) => format!(
                    "streamed {} · rate – (needs two events and a span)",
                    fmt_duration_us(span)
                ),
                _ => "not recorded".to_string(),
            },
            good,
        );

        // Stream closed (EOF after the last token).
        let tail_us = perf.and_then(|p| p.tail_us);
        let eof_us = match (last_token_us, tail_us) {
            (Some(last), Some(tail)) => Some(last.saturating_add(tail)),
            _ => None,
        };
        node(
            &mut lines,
            eof_us.map(|us| base_ms + us as f64 / 1000.0),
            "●",
            "Stream closed",
            tail_us.map_or("not recorded".to_string(), |tail| {
                format!("{} after the last token", fmt_duration_us(tail))
            }),
            muted,
        );

        // Turn end: validated and settled.
        let e2e_us = perf
            .and_then(|p| p.e2e_us)
            .unwrap_or(att.e2e_duration_ms * 1_000);
        node(
            &mut lines,
            Some(base_ms + e2e_us as f64 / 1000.0),
            "■",
            "Turn end",
            format!("validated after {}", fmt_duration_us(e2e_us)),
            warn,
        );

        if perf.and_then(|p| p.rtt_us).is_some() || perf.map(|p| p.retransmits).unwrap_or(0) > 0 {
            let rtt = perf
                .and_then(|p| p.rtt_us)
                .map(fmt_duration_us)
                .unwrap_or_else(|| "–".into());
            let retransmits = perf.map(|p| p.retransmits).unwrap_or(0);
            lines.push(Line::from(vec![
                Span::styled("  ", Style::default()),
                Span::styled(
                    format!("socket: RTT {rtt} · retransmits {retransmits}"),
                    Style::default().fg(theme.text_muted),
                ),
            ]));
        }
    } else {
        lines.push(Line::from(vec![Span::styled(
            "  Attempt record not found.",
            Style::default().fg(theme.text_muted),
        )]));
    }

    lines
}
