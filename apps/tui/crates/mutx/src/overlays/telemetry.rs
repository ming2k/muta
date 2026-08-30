//! Session Telemetry modal: unified context usage and performance telemetry
//! grouped by user round, with turn-level drill-down and attempt inspection.
//!
//! Replaces the previously separated Context Usage and Performance modals with
//! a single, cohesive, terminal-only telemetry inspector.
//!
//! Levels:
//! - L1: Round list (macro business view: Total tokens, Cache %, Stream TPS, Duration, Turns count)
//! - L2: Turn list (micro request view: Turn, Tokens, TTFT, Stream TPS, Duration, Status)
//! - L3: Attempt Inspector (target model@connection, context space bar, vertical timeline waterfall)

use std::collections::BTreeMap;

use muta_contracts::{RequestPerformance, RequestUsageStatus, TokenSourceReport};
use mutx_engine::{Frame, Line, Modifier, Rect, Span, Style};

use super::common::placeholder;
use crate::components::selectable_body::{SelectableRow, render_selectable_body};
use crate::design::MODAL_INNER_H_PADDING;
use crate::model::layout::LayoutMap;
use crate::model::selection::SelectionState;
use crate::primitives::{
    BodyRenderOptions, ContentModalSpec, FooterHint, HeaderPart, breadcrumb_parts,
    content_modal_area, content_modal_probe, hierarchical_breadcrumb, keyvocab, modal_chrome_rows,
    modal_frame, modal_header_parts, render_body, render_modal_footer,
};
use crate::view::Theme;

/// View properties for contextual tokens when displaying context limits.
#[derive(Debug, Clone, Copy, Default)]
#[allow(dead_code)]
pub struct ContextUsageView {
    pub snapshot: Option<muta_contracts::ContextTokenSnapshot>,
    pub window_tokens: Option<usize>,
    pub draft_content_tokens: usize,
    pub draft_tokens: usize,
}

/// A parsed attempt record for telemetry presentation.
#[derive(Debug, Clone)]
pub struct TelemetryAttempt {
    pub round: u64,
    pub turn: u32,
    pub attempt: u32,
    pub model: String,
    pub provider: String,
    pub status: RequestUsageStatus,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub performance: Option<RequestPerformance>,
    pub e2e_duration_ms: u64,
}

/// A round containing terminal attempts.
#[derive(Debug, Clone)]
pub struct TelemetryRound {
    pub round_number: u64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cache_read_tokens: u64,
    pub total_tokens: u64,
    pub turns_count: usize,
    pub streamed_tokens: u64,
    pub stream_duration_us: u64,
    pub e2e_duration_ms: u64,
    pub attempts: Vec<TelemetryAttempt>,
}

impl TelemetryRound {
    pub fn cache_hit_rate(&self) -> f64 {
        if self.prompt_tokens == 0 {
            0.0
        } else {
            (self.cache_read_tokens as f64 / self.prompt_tokens as f64) * 100.0
        }
    }

    pub fn observed_stream_tps(&self) -> Option<f64> {
        if self.stream_duration_us == 0 || self.streamed_tokens == 0 {
            None
        } else {
            Some(self.streamed_tokens as f64 / (self.stream_duration_us as f64 / 1_000_000.0))
        }
    }
}

/// Filter and extract only terminal attempts, grouped by round descending.
pub fn extract_telemetry_rounds(report: &TokenSourceReport) -> Vec<TelemetryRound> {
    let mut round_map = BTreeMap::<u64, Vec<TelemetryAttempt>>::new();

    for row in &report.rows {
        for req in &row.requests {
            // Invariant: Only terminal settled attempts enter telemetry tables.
            if !req.status.is_terminal() {
                continue;
            }

            let prompt = req.prompt_tokens.max(0) as u64;
            let completion = req.completion_tokens.max(0) as u64;
            let cache_read = req.cache_read_tokens.max(0) as u64;
            let cache_write = req.cache_write_tokens.max(0) as u64;

            let e2e_duration_ms = req
                .performance
                .and_then(|p| p.e2e_us.map(|us| us / 1_000))
                .unwrap_or(req.generation_ms);

            let attempt = TelemetryAttempt {
                round: req.key.round,
                turn: req.key.turn,
                attempt: req.key.attempt,
                model: req.model.clone(),
                provider: req.provider.clone(),
                status: req.status,
                prompt_tokens: prompt,
                completion_tokens: completion,
                cache_read_tokens: cache_read,
                cache_write_tokens: cache_write,
                performance: req.performance,
                e2e_duration_ms,
            };

            round_map.entry(req.key.round).or_default().push(attempt);
        }
    }

    let mut result = Vec::new();
    // Descending order: latest round first
    for (round_num, mut attempts) in round_map.into_iter().rev() {
        // Sort attempts within round by (turn asc, attempt asc)
        attempts.sort_by_key(|a| (a.turn, a.attempt));

        let mut prompt_tokens = 0u64;
        let mut completion_tokens = 0u64;
        let mut cache_read_tokens = 0u64;
        let mut streamed_tokens = 0u64;
        let mut stream_duration_us = 0u64;
        let mut e2e_duration_ms = 0u64;

        let mut distinct_turns = std::collections::BTreeSet::new();

        for att in &attempts {
            prompt_tokens += att.prompt_tokens;
            completion_tokens += att.completion_tokens;
            cache_read_tokens += att.cache_read_tokens;
            distinct_turns.insert(att.turn);
            e2e_duration_ms += att.e2e_duration_ms;

            if let Some(p) = att.performance {
                if let Some(dur_us) = p.stream_us {
                    if dur_us > 0 && p.streamed_output_tokens > 0 {
                        streamed_tokens += p.streamed_output_tokens;
                        stream_duration_us += dur_us;
                    }
                }
            }
        }

        let total_tokens = prompt_tokens + completion_tokens;
        let turns_count = distinct_turns.len();

        result.push(TelemetryRound {
            round_number: round_num,
            prompt_tokens,
            completion_tokens,
            cache_read_tokens,
            total_tokens,
            turns_count,
            streamed_tokens,
            stream_duration_us,
            e2e_duration_ms,
            attempts,
        });
    }

    result
}

/// Returns the number of distinct rounds in the report.
pub fn telemetry_round_count(report: &TokenSourceReport) -> usize {
    extract_telemetry_rounds(report).len()
}

/// Returns the number of attempts for the given round index (0-based, descending).
pub fn telemetry_attempt_count(report: &TokenSourceReport, round_index: usize) -> usize {
    let rounds = extract_telemetry_rounds(report);
    rounds.get(round_index).map_or(0, |r| r.attempts.len())
}

/// Returns the (round_number, attempt_index) key for a given round index and attempt index.
pub fn telemetry_attempt_key(
    report: &TokenSourceReport,
    round_index: usize,
    attempt_index: usize,
) -> Option<(u32, u32)> {
    let rounds = extract_telemetry_rounds(report);
    let round = rounds.get(round_index)?;
    let attempt = round.attempts.get(attempt_index)?;
    Some((attempt.round as u32, attempt.attempt))
}

// ─────────────────────────────────────────────────────────────────────────────
// Formatters
// ─────────────────────────────────────────────────────────────────────────────

fn fmt_tokens(count: u64) -> String {
    if count >= 1_000_000 {
        format!("{:.1}M", count as f64 / 1_000_000.0)
    } else if count >= 1_000 {
        format!("{:.1}k", count as f64 / 1_000.0)
    } else {
        format!("{count}")
    }
}

fn fmt_duration_ms(ms: u64) -> String {
    if ms >= 60_000 {
        let mins = ms / 60_000;
        let secs = (ms % 60_000) as f64 / 1_000.0;
        format!("{mins}m {secs:.1}s")
    } else if ms >= 1_000 {
        format!("{:.1}s", ms as f64 / 1_000.0)
    } else {
        format!("{ms}ms")
    }
}

fn fmt_duration_us(us: u64) -> String {
    if us >= 1_000_000 {
        format!("{:.2}s", us as f64 / 1_000_000.0)
    } else if us >= 1_000 {
        format!("{:.0}ms", us as f64 / 1_000.0)
    } else {
        format!("{us}µs")
    }
}

fn fmt_tps(tps: Option<f64>) -> String {
    match tps {
        Some(rate) if rate > 0.0 => format!("{rate:.1} tok/s"),
        _ => "--".to_string(),
    }
}

fn status_style(status: RequestUsageStatus, theme: &Theme) -> Style {
    match status {
        RequestUsageStatus::Completed => Style::default().fg(theme.success),
        RequestUsageStatus::Interrupted => Style::default().fg(theme.warning),
        RequestUsageStatus::Failed | RequestUsageStatus::Abandoned => {
            Style::default().fg(theme.error_fg)
        }
        RequestUsageStatus::InFlight => Style::default().fg(theme.brand()),
    }
}

fn status_label(status: RequestUsageStatus) -> &'static str {
    match status {
        RequestUsageStatus::Completed => "Done",
        RequestUsageStatus::Interrupted => "Interrupted",
        RequestUsageStatus::Failed => "Failed",
        RequestUsageStatus::Abandoned => "Abandoned",
        RequestUsageStatus::InFlight => "In-flight",
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Modal Renderer
// ─────────────────────────────────────────────────────────────────────────────

/// Draw the unified Session Telemetry modal.
#[allow(clippy::too_many_arguments)]
pub fn draw_telemetry_modal(
    frame: &mut Frame,
    report: &TokenSourceReport,
    context: ContextUsageView,
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
                &[FooterHint::always(keyvocab::ESC, "close")],
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

    let (header, body, footer, follow, is_doc) = if let Some((target_round, target_attempt)) = turn {
        let levels = ["Session Telemetry", round_child.as_str(), turn_child.as_str()];
        let header = hierarchical_breadcrumb(&levels, header_width);
        let body = build_attempt_inspector_body(&rounds, target_round, target_attempt, context, body_width, theme);
        let footer = vec![
            FooterHint::always(keyvocab::ARROWS_UD, "scroll"),
            FooterHint::always(keyvocab::ESC, "turns"),
        ];
        (header, body, footer, None, true)
    } else if detail {
        let header = breadcrumb_parts("Session Telemetry", &round_child).to_vec();
        let (body, follow) = build_turns_body(&rounds, selected, turn_cursor, body_width, theme);
        let footer = vec![
            FooterHint::always(keyvocab::ARROWS_UD, "select"),
            FooterHint::always(keyvocab::ENTER, "inspect"),
            FooterHint::always(keyvocab::ESC, "rounds"),
        ];
        (header, body, footer, follow, true)
    } else {
        let header = vec![HeaderPart::title("Session Telemetry")];
        let (body, follow) = build_rounds_body(&rounds, selected, body_width, theme);
        let footer = vec![
            FooterHint::always(keyvocab::ARROWS_UD, "select"),
            FooterHint::always(keyvocab::ENTER, "turns"),
            FooterHint::always(keyvocab::ESC, "close"),
        ];
        (header, body, footer, follow, false)
    };

    let desired = body.len() as u16 + modal_chrome_rows(geometry.modal_spec());
    let area = content_modal_area(frame, geometry, desired);
    let modal = modal_frame(frame, area, theme.panel(), true, true);

    modal_header_parts(frame, modal.header, &header, theme);

    if is_doc {
        let rows: Vec<SelectableRow> = body.into_iter().map(SelectableRow::from_line).collect();
        render_selectable_body(
            frame,
            modal.body,
            &rows,
            scroll,
            follow,
            theme,
            selection,
            layout_map,
        );
    } else {
        render_body(
            frame,
            modal.body,
            body,
            scroll,
            BodyRenderOptions::follow(follow),
            theme,
        );
    }

    if let Some(footer_area) = modal.footer {
        render_modal_footer(frame, footer_area, &footer, theme);
    }
    area
}

// ─────────────────────────────────────────────────────────────────────────────
// L1: Round List Body
// ─────────────────────────────────────────────────────────────────────────────

fn build_rounds_body(
    rounds: &[TelemetryRound],
    selected_idx: usize,
    width: usize,
    theme: &Theme,
) -> (Vec<Line<'static>>, Option<usize>) {
    let mut lines = Vec::new();

    if rounds.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::styled("  No settled turns recorded in this session yet.", Style::default().fg(theme.text_muted)),
        ]));
        return (lines, None);
    }

    // Responsive column layout based on available width
    let show_cache = width >= 86;
    let show_turns = width >= 72;

    let col_round = 8;
    let col_tokens = if width >= 76 { 20 } else { 14 };
    let col_cache = if show_cache { 12 } else { 0 };
    let col_tps = 15;
    let col_dur = 11;
    let col_turns = if show_turns { 10 } else { 0 };

    // Column Headers
    let mut header_spans = vec![
        Span::styled(format!("  {:<w$}", "Round", w = col_round), Style::default().fg(theme.text_muted).add_modifier(Modifier::BOLD)),
        Span::styled(
            if width >= 76 {
                format!("{:<w$}", "Tokens (In / Out)", w = col_tokens)
            } else {
                format!("{:<w$}", "Tokens", w = col_tokens)
            },
            Style::default().fg(theme.text_muted).add_modifier(Modifier::BOLD),
        ),
    ];
    if show_cache {
        header_spans.push(Span::styled(format!("{:<w$}", "Cache %", w = col_cache), Style::default().fg(theme.text_muted).add_modifier(Modifier::BOLD)));
    }
    header_spans.push(Span::styled(format!("{:<w$}", "Stream TPS", w = col_tps), Style::default().fg(theme.text_muted).add_modifier(Modifier::BOLD)));
    header_spans.push(Span::styled(format!("{:<w$}", "Duration", w = col_dur), Style::default().fg(theme.text_muted).add_modifier(Modifier::BOLD)));
    if show_turns {
        header_spans.push(Span::styled(format!("{:<w$}", "Turns", w = col_turns), Style::default().fg(theme.text_muted).add_modifier(Modifier::BOLD)));
    }
    lines.push(Line::from(header_spans));

    // Separator
    let sep_len = width.saturating_sub(4);
    lines.push(Line::from(vec![
        Span::styled(format!("  {}", "─".repeat(sep_len)), Style::default().fg(theme.dim())),
    ]));

    let mut follow = None;
    for (i, r) in rounds.iter().enumerate() {
        let is_selected = i == selected_idx;
        if is_selected {
            follow = Some(lines.len());
        }
        let marker = if is_selected { "▸ " } else { "  " };

        let round_label = format!("#{:<w$}", r.round_number, w = col_round - 1);
        let tokens_label = if width >= 76 {
            format!("{:<w$}", format!("{} / {}", fmt_tokens(r.prompt_tokens), fmt_tokens(r.completion_tokens)), w = col_tokens)
        } else {
            format!("{:<w$}", fmt_tokens(r.total_tokens), w = col_tokens)
        };
        let cache_label = format!("{:<w$}", format!("{:.1}%", r.cache_hit_rate()), w = col_cache);
        let tps_label = format!("{:<w$}", fmt_tps(r.observed_stream_tps()), w = col_tps);
        let dur_label = format!("{:<w$}", fmt_duration_ms(r.e2e_duration_ms), w = col_dur);
        let turns_label = format!("{:<w$}", format!("{} turn{}", r.turns_count, if r.turns_count == 1 { "" } else { "s" }), w = col_turns);

        let row_style = if is_selected {
            Style::default().fg(theme.brand()).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.text)
        };

        let mut row_spans = vec![
            Span::styled(marker, if is_selected { Style::default().fg(theme.brand()) } else { Style::default().fg(theme.text_muted) }),
            Span::styled(round_label, row_style),
            Span::styled(tokens_label, row_style),
        ];
        if show_cache {
            row_spans.push(Span::styled(
                cache_label,
                if r.cache_hit_rate() > 0.0 { Style::default().fg(theme.success) } else { Style::default().fg(theme.text_muted) },
            ));
        }
        row_spans.push(Span::styled(tps_label, row_style));
        row_spans.push(Span::styled(dur_label, Style::default().fg(theme.text_muted)));
        if show_turns {
            row_spans.push(Span::styled(turns_label, Style::default().fg(theme.text_muted)));
        }

        lines.push(Line::from(row_spans));
    }

    (lines, follow)
}

// ─────────────────────────────────────────────────────────────────────────────
// L2: Turn List Body
// ─────────────────────────────────────────────────────────────────────────────

fn build_turns_body(
    rounds: &[TelemetryRound],
    selected_round_idx: usize,
    selected_turn_idx: usize,
    width: usize,
    theme: &Theme,
) -> (Vec<Line<'static>>, Option<usize>) {
    let round = rounds.get(selected_round_idx);
    let mut lines = Vec::new();

    let show_dur = width >= 86;

    let col_turn = 12;
    let col_tokens = if width >= 76 { 20 } else { 14 };
    let col_ttft = 12;
    let col_tps = 15;
    let col_dur = if show_dur { 11 } else { 0 };
    let col_status = 10;

    // Headers
    let mut header_spans = vec![
        Span::styled(format!("  {:<w$}", "Turn", w = col_turn), Style::default().fg(theme.text_muted).add_modifier(Modifier::BOLD)),
        Span::styled(
            if width >= 76 {
                format!("{:<w$}", "Tokens (In / Out)", w = col_tokens)
            } else {
                format!("{:<w$}", "Tokens", w = col_tokens)
            },
            Style::default().fg(theme.text_muted).add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!("{:<w$}", "TTFT", w = col_ttft), Style::default().fg(theme.text_muted).add_modifier(Modifier::BOLD)),
        Span::styled(format!("{:<w$}", "Stream TPS", w = col_tps), Style::default().fg(theme.text_muted).add_modifier(Modifier::BOLD)),
    ];
    if show_dur {
        header_spans.push(Span::styled(format!("{:<w$}", "Duration", w = col_dur), Style::default().fg(theme.text_muted).add_modifier(Modifier::BOLD)));
    }
    header_spans.push(Span::styled(format!("{:<w$}", "Status", w = col_status), Style::default().fg(theme.text_muted).add_modifier(Modifier::BOLD)));
    lines.push(Line::from(header_spans));

    // Separator
    let sep_len = width.saturating_sub(4);
    lines.push(Line::from(vec![
        Span::styled(format!("  {}", "─".repeat(sep_len)), Style::default().fg(theme.dim())),
    ]));

    let mut follow = None;
    if let Some(r) = round {
        for (i, att) in r.attempts.iter().enumerate() {
            let is_selected = i == selected_turn_idx;
            if is_selected {
                follow = Some(lines.len());
            }
            let marker = if is_selected { "▸ " } else { "  " };

            let turn_label = format!("Turn {}.{:<w$}", att.round, att.turn, w = col_turn.saturating_sub(8));
            let tokens_label = if width >= 76 {
                format!("{:<w$}", format!("{} / {}", fmt_tokens(att.prompt_tokens), fmt_tokens(att.completion_tokens)), w = col_tokens)
            } else {
                format!("{:<w$}", fmt_tokens(att.prompt_tokens + att.completion_tokens), w = col_tokens)
            };

            let ttft_str = att.performance
                .and_then(|p| p.ttft_us)
                .map(fmt_duration_us)
                .unwrap_or_else(|| "--".to_string());
            let ttft_label = format!("{:<w$}", ttft_str, w = col_ttft);

            let tps_str = fmt_tps(att.performance.and_then(|p| p.observed_stream_tps()));
            let tps_label = format!("{:<w$}", tps_str, w = col_tps);

            let dur_label = format!("{:<w$}", fmt_duration_ms(att.e2e_duration_ms), w = col_dur);
            let status_lbl = status_label(att.status);
            let status_st = status_style(att.status, theme);

            let row_style = if is_selected {
                Style::default().fg(theme.brand()).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.text)
            };

            let mut row_spans = vec![
                Span::styled(marker, if is_selected { Style::default().fg(theme.brand()) } else { Style::default().fg(theme.text_muted) }),
                Span::styled(turn_label, row_style),
                Span::styled(tokens_label, row_style),
                Span::styled(ttft_label, row_style),
                Span::styled(tps_label, row_style),
            ];
            if show_dur {
                row_spans.push(Span::styled(dur_label, Style::default().fg(theme.text_muted)));
            }
            row_spans.push(Span::styled(status_lbl, status_st));

            lines.push(Line::from(row_spans));
        }
    }

    (lines, follow)
}

// ─────────────────────────────────────────────────────────────────────────────
// L3: Attempt Inspector Body (Vertical Waterfall)
// ─────────────────────────────────────────────────────────────────────────────

fn build_attempt_inspector_body(
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
            Span::styled(format!("{} @ {}", att.model, connection_display), Style::default().fg(theme.brand()).add_modifier(Modifier::BOLD)),
        ]));
        lines.push(Line::from(vec![
            Span::styled(" Status:  ", Style::default().fg(theme.text_muted)),
            Span::styled(format!("{:?}", att.status), status_style(att.status, theme).add_modifier(Modifier::BOLD)),
            Span::raw("   "),
            Span::styled("Attempt: ", Style::default().fg(theme.text_muted)),
            Span::styled(format!("Turn {}.{} (attempt #{})", att.round, att.turn, att.attempt), Style::default().fg(theme.text)),
        ]));
        lines.push(Line::from(""));

        // Context Space Section
        lines.push(Line::from(vec![
            Span::styled("── Context Space ────────────────────────────────────────────────────────", Style::default().fg(theme.dim())),
        ]));

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
            Span::styled("  Input Context:     ", Style::default().fg(theme.text_muted)),
            Span::styled(format!("{:<10}", fmt_tokens(att.prompt_tokens)), Style::default().fg(theme.text).add_modifier(Modifier::BOLD)),
            Span::styled(format!("{bar} {ctx_pct:.1}% of {} max", fmt_tokens(window_max as u64)), Style::default().fg(theme.text_muted)),
        ]));

        lines.push(Line::from(vec![
            Span::styled("   ├─ Cached Read:   ", Style::default().fg(theme.text_muted)),
            Span::styled(format!("{:<10}", fmt_tokens(att.cache_read_tokens)), Style::default().fg(if att.cache_read_tokens > 0 { theme.success } else { theme.text })),
            Span::styled(format!("({cache_pct:.1}% Cache Hit)"), Style::default().fg(if cache_pct > 0.0 { theme.success } else { theme.text_muted })),
        ]));

        let fresh_input = att.prompt_tokens.saturating_sub(att.cache_read_tokens);
        lines.push(Line::from(vec![
            Span::styled("   ├─ Fresh Input:   ", Style::default().fg(theme.text_muted)),
            Span::styled(format!("{:<10}", fmt_tokens(fresh_input)), Style::default().fg(theme.text)),
        ]));

        if att.cache_write_tokens > 0 {
            lines.push(Line::from(vec![
                Span::styled("   ├─ Cache Created: ", Style::default().fg(theme.text_muted)),
                Span::styled(format!("{:<10}", fmt_tokens(att.cache_write_tokens)), Style::default().fg(theme.warning)),
            ]));
        }

        lines.push(Line::from(vec![
            Span::styled("  Output Generated:  ", Style::default().fg(theme.text_muted)),
            Span::styled(format!("{:<10}", fmt_tokens(att.completion_tokens)), Style::default().fg(theme.text).add_modifier(Modifier::BOLD)),
        ]));
        lines.push(Line::from(""));

        // Latency Timeline Waterfall Section
        lines.push(Line::from(vec![
            Span::styled("── Latency Timeline Waterfall ───────────────────────────────────────────", Style::default().fg(theme.dim())),
        ]));
        lines.push(Line::from(""));

        let perf = att.performance;

        // Node 0: Request Dispatched
        lines.push(Line::from(vec![
            Span::styled("  ● 0.00s  ", Style::default().fg(theme.brand()).add_modifier(Modifier::BOLD)),
            Span::styled("Request Dispatched", Style::default().fg(theme.text).add_modifier(Modifier::BOLD)),
        ]));
        lines.push(Line::from(vec![Span::styled("  │", Style::default().fg(theme.dim()))]));

        // Branch 1: Connect & Handshake
        let ready_us = perf.and_then(|p| p.stream_ready_us);
        let ready_str = ready_us.map_or("--".to_string(), fmt_duration_us);
        lines.push(Line::from(vec![
            Span::styled("  │ ┌─ Connect & Handshake ", Style::default().fg(theme.dim())),
            Span::styled(format!("─── {ready_str}"), Style::default().fg(theme.brand())),
        ]));
        lines.push(Line::from(vec![
            Span::styled("  │ │  DNS + TLS + Gateway + Send Request Payload", Style::default().fg(theme.text_muted)),
        ]));
        lines.push(Line::from(vec![
            Span::styled("  │ └────────────────────────────────────────────────", Style::default().fg(theme.dim())),
        ]));
        lines.push(Line::from(vec![Span::styled("  │", Style::default().fg(theme.dim()))]));

        // Node 1: Stream Ready
        let ready_node_time = ready_us.map_or("+--".to_string(), |us| format!("+{:.2}s", us as f64 / 1_000_000.0));
        lines.push(Line::from(vec![
            Span::styled(format!("  ● {ready_node_time}  "), Style::default().fg(theme.brand()).add_modifier(Modifier::BOLD)),
            Span::styled("Stream Ready", Style::default().fg(theme.text).add_modifier(Modifier::BOLD)),
            Span::styled(" (HTTP 200 Headers received)", Style::default().fg(theme.text_muted)),
        ]));
        lines.push(Line::from(vec![Span::styled("  │", Style::default().fg(theme.dim()))]));

        // Branch 2: Prefill & Server Queue
        let ttft_us = perf.and_then(|p| p.ttft_us);
        let prefill_us = match (ttft_us, ready_us) {
            (Some(ttft), Some(ready)) => Some(ttft.saturating_sub(ready)),
            _ => None,
        };
        let prefill_str = prefill_us.map_or("--".to_string(), fmt_duration_us);
        lines.push(Line::from(vec![
            Span::styled("  │ ┌─ Prefill & Server Queue ", Style::default().fg(theme.dim())),
            Span::styled(format!("──── {prefill_str}"), Style::default().fg(theme.warning)),
        ]));
        lines.push(Line::from(vec![
            Span::styled(
                format!("  │ │  Prompt Processing ({} cached / {} eval)", fmt_tokens(att.cache_read_tokens), fmt_tokens(fresh_input)),
                Style::default().fg(theme.text_muted),
            ),
        ]));
        lines.push(Line::from(vec![
            Span::styled("  │ └────────────────────────────────────────────────", Style::default().fg(theme.dim())),
        ]));
        lines.push(Line::from(vec![Span::styled("  │", Style::default().fg(theme.dim()))]));

        // Node 2: First Token Arrived (TTFT)
        let ttft_node_time = ttft_us.map_or("+--".to_string(), |us| format!("+{:.2}s", us as f64 / 1_000_000.0));
        let ttft_val_str = ttft_us.map_or("--".to_string(), fmt_duration_us);
        lines.push(Line::from(vec![
            Span::styled(format!("  ● {ttft_node_time}  "), Style::default().fg(theme.success).add_modifier(Modifier::BOLD)),
            Span::styled("First Token Arrived", Style::default().fg(theme.text).add_modifier(Modifier::BOLD)),
            Span::styled(format!(" (Client TTFT: {ttft_val_str})"), Style::default().fg(theme.success)),
        ]));
        lines.push(Line::from(vec![Span::styled("  │", Style::default().fg(theme.dim()))]));

        // Branch 3: Stream Decode
        let stream_us = perf.and_then(|p| p.stream_us);
        let stream_str = stream_us.map_or("--".to_string(), fmt_duration_us);
        let stream_tps_str = fmt_tps(perf.and_then(|p| p.observed_stream_tps()));
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
            Span::styled("  │ ┌─ Stream Decode (Generating) ", Style::default().fg(theme.dim())),
            Span::styled(format!("─── {stream_str}"), Style::default().fg(theme.success)),
        ]));
        lines.push(Line::from(vec![
            Span::styled(
                format!("  │ │  {streamed_tok_str} tokens generated @ {stream_tps_str}"),
                Style::default().fg(theme.text_muted),
            ),
        ]));
        lines.push(Line::from(vec![
            Span::styled("  │ └────────────────────────────────────────────────", Style::default().fg(theme.dim())),
        ]));
        lines.push(Line::from(vec![Span::styled("  │", Style::default().fg(theme.dim()))]));

        // Node 3: Last Token Received
        let last_token_us = match (ttft_us, stream_us) {
            (Some(ttft), Some(stream)) => Some(ttft + stream),
            _ => None,
        };
        let last_node_time = last_token_us.map_or("+--".to_string(), |us| format!("+{:.2}s", us as f64 / 1_000_000.0));
        lines.push(Line::from(vec![
            Span::styled(format!("  ● {last_node_time}  "), Style::default().fg(theme.brand()).add_modifier(Modifier::BOLD)),
            Span::styled("Last Token Received", Style::default().fg(theme.text).add_modifier(Modifier::BOLD)),
        ]));
        lines.push(Line::from(vec![Span::styled("  │", Style::default().fg(theme.dim()))]));

        // Branch 4: Tail & Commit
        let tail_us = perf.and_then(|p| p.tail_us);
        let tail_str = tail_us.map_or("--".to_string(), fmt_duration_us);
        lines.push(Line::from(vec![
            Span::styled("  │ ┌─ Tail & Commit ", Style::default().fg(theme.dim())),
            Span::styled(format!("────────────── {tail_str}"), Style::default().fg(theme.text_muted)),
        ]));
        lines.push(Line::from(vec![
            Span::styled("  │ │  Stream EOF verification + schema parse", Style::default().fg(theme.text_muted)),
        ]));
        lines.push(Line::from(vec![
            Span::styled("  │ └────────────────────────────────────────────────", Style::default().fg(theme.dim())),
        ]));
        lines.push(Line::from(vec![Span::styled("  │", Style::default().fg(theme.dim()))]));

        // Node 4: Final Completed
        let e2e_us = perf.and_then(|p| p.e2e_us).unwrap_or(att.e2e_duration_ms * 1_000);
        let e2e_str = fmt_duration_us(e2e_us);
        lines.push(Line::from(vec![
            Span::styled(format!("  ■ +{e2e_str}  "), Style::default().fg(theme.brand()).add_modifier(Modifier::BOLD)),
            Span::styled("Request Completed", Style::default().fg(theme.text).add_modifier(Modifier::BOLD)),
            Span::styled(format!(" (Total E2E: {e2e_str})"), Style::default().fg(theme.brand())),
        ]));
    } else {
        lines.push(Line::from(vec![
            Span::styled("  Attempt record not found.", Style::default().fg(theme.text_muted)),
        ]));
    }

    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use muta_contracts::{
        RequestPerformance, RequestUsageKey, RequestUsageRecord, RequestUsageSource,
        RequestUsageStatus, TokenSourceReport, TokenSourceRow,
    };

    #[test]
    fn test_extract_telemetry_rounds_filters_terminal_only() {
        let mut report = TokenSourceReport::default();
        let row = TokenSourceRow {
            provider: "anthropic".to_string(),
            model: "claude-3-7-sonnet".to_string(),
            turns: Vec::new(),
            requests: vec![
                RequestUsageRecord {
                    key: RequestUsageKey {
                        session_id: "s1".to_string(),
                        round: 1,
                        turn: 1,
                        attempt: 1,
                        actor_id: "master".to_string(),
                    },
                    provider: "anthropic".to_string(),
                    model: "claude-3-7-sonnet".to_string(),
                    status: RequestUsageStatus::InFlight, // Non-terminal
                    source: RequestUsageSource::Reported,
                    prompt_tokens: 100,
                    completion_tokens: 10,
                    total_tokens: 110,
                    generation_ms: 500,
                    ..Default::default()
                },
                RequestUsageRecord {
                    key: RequestUsageKey {
                        session_id: "s1".to_string(),
                        round: 1,
                        turn: 1,
                        attempt: 2,
                        actor_id: "master".to_string(),
                    },
                    provider: "anthropic".to_string(),
                    model: "claude-3-7-sonnet".to_string(),
                    status: RequestUsageStatus::Completed, // Terminal
                    source: RequestUsageSource::Reported,
                    prompt_tokens: 1000,
                    completion_tokens: 200,
                    cache_read_tokens: 800,
                    total_tokens: 1200,
                    generation_ms: 1500,
                    performance: Some(RequestPerformance {
                        stream_ready_us: Some(100_000),
                        ttft_us: Some(300_000),
                        stream_us: Some(1_200_000),
                        tail_us: Some(20_000),
                        e2e_us: Some(1_520_000),
                        streamed_output_tokens: 200,
                        first_output_tokens: 1,
                        output_events: 50,
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            ],
            totals: Default::default(),
        };
        report.rows.push(row);

        let rounds = extract_telemetry_rounds(&report);
        assert_eq!(rounds.len(), 1);
        let r1 = &rounds[0];
        assert_eq!(r1.round_number, 1);
        assert_eq!(r1.attempts.len(), 1); // Running attempt filtered out!
        assert_eq!(r1.prompt_tokens, 1000);
        assert_eq!(r1.completion_tokens, 200);
        assert_eq!(r1.cache_read_tokens, 800);
        assert_eq!(r1.cache_hit_rate(), 80.0);
        assert!(r1.observed_stream_tps().is_some());
    }

    #[test]
    fn test_telemetry_round_and_turn_helpers() {
        let mut report = TokenSourceReport::default();
        let row = TokenSourceRow {
            provider: "anthropic".to_string(),
            model: "claude-3-7-sonnet".to_string(),
            turns: Vec::new(),
            requests: vec![
                RequestUsageRecord {
                    key: RequestUsageKey {
                        session_id: "s1".to_string(),
                        round: 2,
                        turn: 1,
                        attempt: 1,
                        actor_id: "master".to_string(),
                    },
                    provider: "anthropic".to_string(),
                    model: "claude-3-7-sonnet".to_string(),
                    status: RequestUsageStatus::Completed,
                    source: RequestUsageSource::Reported,
                    prompt_tokens: 2000,
                    completion_tokens: 150,
                    cache_read_tokens: 1600,
                    total_tokens: 2150,
                    generation_ms: 1200,
                    performance: Some(RequestPerformance {
                        stream_ready_us: Some(150_000),
                        ttft_us: Some(350_000),
                        stream_us: Some(1_000_000),
                        tail_us: Some(30_000),
                        e2e_us: Some(1_200_000),
                        streamed_output_tokens: 150,
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                RequestUsageRecord {
                    key: RequestUsageKey {
                        session_id: "s1".to_string(),
                        round: 1,
                        turn: 1,
                        attempt: 1,
                        actor_id: "master".to_string(),
                    },
                    provider: "anthropic".to_string(),
                    model: "claude-3-7-sonnet".to_string(),
                    status: RequestUsageStatus::Completed,
                    source: RequestUsageSource::Reported,
                    prompt_tokens: 500,
                    completion_tokens: 50,
                    total_tokens: 550,
                    generation_ms: 600,
                    ..Default::default()
                },
            ],
            totals: Default::default(),
        };
        report.rows.push(row);

        assert_eq!(telemetry_round_count(&report), 2);
        // Round 2 is first (descending)
        assert_eq!(telemetry_attempt_count(&report, 0), 1);
        assert_eq!(telemetry_attempt_count(&report, 1), 1);
        assert_eq!(telemetry_attempt_key(&report, 0, 0), Some((2, 1)));
        assert_eq!(telemetry_attempt_key(&report, 1, 0), Some((1, 1)));
    }

    #[test]
    fn test_build_attempt_inspector_waterfall_nodes() {
        let theme = Theme::from_color_scheme("dark", &Default::default());
        let rounds = vec![TelemetryRound {
            round_number: 1,
            prompt_tokens: 4000,
            completion_tokens: 300,
            cache_read_tokens: 3000,
            total_tokens: 4300,
            turns_count: 1,
            streamed_tokens: 300,
            stream_duration_us: 3_000_000,
            e2e_duration_ms: 3_500,
            attempts: vec![TelemetryAttempt {
                round: 1,
                turn: 1,
                attempt: 1,
                model: "claude-3-7-sonnet".to_string(),
                provider: "anthropic".to_string(),
                status: RequestUsageStatus::Completed,
                prompt_tokens: 4000,
                completion_tokens: 300,
                cache_read_tokens: 3000,
                cache_write_tokens: 0,
                performance: Some(RequestPerformance {
                    stream_ready_us: Some(120_000),
                    ttft_us: Some(280_000),
                    stream_us: Some(3_000_000),
                    tail_us: Some(25_000),
                    e2e_us: Some(3_425_000),
                    streamed_output_tokens: 300,
                    ..Default::default()
                }),
                e2e_duration_ms: 3500,
            }],
        }];

        let lines = build_attempt_inspector_body(
            &rounds,
            1,
            1,
            ContextUsageView {
                window_tokens: Some(200_000),
                ..Default::default()
            },
            100,
            &theme,
        );

        let full_text: String = lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<Vec<_>>().join(""))
            .collect::<Vec<_>>()
            .join("\n");

        assert!(full_text.contains("Target:  claude-3-7-sonnet @ anthropic"));
        assert!(full_text.contains("Context Space"));
        assert!(full_text.contains("75.0% Cache Hit"));
        assert!(full_text.contains("Latency Timeline Waterfall"));
        assert!(full_text.contains("Request Dispatched"));
        assert!(full_text.contains("Connect & Handshake"));
        assert!(full_text.contains("Stream Ready"));
        assert!(full_text.contains("Prefill & Server Queue"));
        assert!(full_text.contains("First Token Arrived"));
        assert!(full_text.contains("Stream Decode"));
        assert!(full_text.contains("Last Token Received"));
        assert!(full_text.contains("Tail & Commit"));
        assert!(full_text.contains("Request Completed"));
    }
}
