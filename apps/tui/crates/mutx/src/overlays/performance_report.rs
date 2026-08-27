//! Request-performance modal: client-observed latency and streaming pace,
//! grouped by user round with a per-turn/attempt drill-down.
//!
//! This surface deliberately owns no context/token-budget presentation. It
//! reads the shared attempt ledger as a durable fact source, then applies
//! performance-specific success filtering and aggregation.

use std::collections::BTreeMap;

use muta_contracts::{
    RequestUsageRecord, RequestUsageSource, RequestUsageStatus, TokenSourceReport,
};
use mutx_engine::{
    Frame, Modifier, Style, {Line, Span},
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::common::placeholder;
use crate::components::selectable_body::{SelectableRow, render_selectable_body};
use crate::design::MODAL_INNER_H_PADDING;
use crate::primitives::{
    BodyRenderOptions, ContentModalSpec, FooterHint, HeaderPart, SCROLL_EDGE_MARGIN,
    breadcrumb_parts, content_modal_area, content_modal_probe, keyvocab, modal_chrome_rows,
    modal_frame, modal_header_parts, render_body, render_modal_footer,
};
use crate::view::Theme;

#[derive(Debug)]
struct PerformanceRound<'a> {
    number: u64,
    attempts: Vec<&'a RequestUsageRecord>,
}

pub fn performance_report_round_count(report: &TokenSourceReport) -> usize {
    performance_rounds(report).len()
}

#[allow(clippy::too_many_arguments)]
pub fn draw_performance_report_modal(
    frame: &mut Frame,
    report: &TokenSourceReport,
    selected: usize,
    detail: bool,
    loading: bool,
    scroll: &mut usize,
    theme: &Theme,
    selection: &crate::model::selection::SelectionState,
    layout_map: &mut crate::model::layout::LayoutMap,
) -> mutx_engine::Rect {
    let geometry = ContentModalSpec::TOKEN_REPORT;
    let probe = content_modal_probe(frame, geometry);
    let body_width = (probe.width as usize)
        .saturating_sub(2 * MODAL_INNER_H_PADDING as usize)
        .max(1);
    let rounds = performance_rounds(report);
    let round_count = rounds.len();
    let drill = detail && round_count > 0;
    let selected = selected.min(round_count.saturating_sub(1));
    let child = drill
        .then(|| round_label(rounds[selected].number))
        .unwrap_or_default();

    let (header, body, footer, follow) = if loading && !drill {
        (
            vec![HeaderPart::title("Performance")],
            vec![placeholder(
                "Loading request performance from the daemon…",
                true,
                theme.muted(),
            )],
            vec![FooterHint::always(keyvocab::ESC, "close")],
            None,
        )
    } else if drill {
        (
            breadcrumb_parts("Performance", &child).to_vec(),
            detail_body(&rounds[selected], body_width, theme),
            vec![
                FooterHint::always(keyvocab::ARROWS_UD, "scroll"),
                FooterHint::always(keyvocab::ESC, "rounds"),
            ],
            None,
        )
    } else {
        let (body, follow) = list_body(&rounds, selected, body_width, theme);
        let footer = if round_count == 0 {
            vec![FooterHint::always(keyvocab::ESC, "close")]
        } else {
            vec![
                FooterHint::always(keyvocab::ARROWS_UD, "select"),
                FooterHint::always(keyvocab::ENTER, "turns"),
                FooterHint::always(keyvocab::ESC, "close"),
            ]
        };
        (vec![HeaderPart::title("Performance")], body, footer, follow)
    };

    let desired = body.len() as u16 + modal_chrome_rows(geometry.modal_spec());
    let area = content_modal_area(frame, geometry, desired);
    let modal = modal_frame(frame, area, theme.panel(), true, true);
    modal_header_parts(frame, modal.header, &header, theme);
    if drill {
        let rows = body
            .into_iter()
            .map(SelectableRow::from_line)
            .collect::<Vec<_>>();
        render_selectable_body(
            frame, modal.body, &rows, scroll, follow, theme, selection, layout_map,
        );
    } else {
        render_body(
            frame,
            modal.body,
            body,
            scroll,
            BodyRenderOptions::new(
                follow,
                if follow.is_some() {
                    SCROLL_EDGE_MARGIN
                } else {
                    0
                },
                false,
            ),
            theme,
        );
    }
    if let Some(footer_area) = modal.footer {
        render_modal_footer(frame, footer_area, &footer, theme);
    }
    area
}

fn list_body(
    rounds: &[PerformanceRound<'_>],
    selected: usize,
    body_width: usize,
    theme: &Theme,
) -> (Vec<Line<'static>>, Option<usize>) {
    let mut body = Vec::new();
    let successful = rounds
        .iter()
        .flat_map(|round| round.attempts.iter().copied())
        .filter(|record| record.status == RequestUsageStatus::Completed)
        .collect::<Vec<_>>();
    let mut ttfts = successful
        .iter()
        .filter_map(|record| observed_ttft_us(record))
        .collect::<Vec<_>>();
    ttfts.sort_unstable();

    body.push(kv(
        "TTFT",
        &match (percentile(&ttfts, 50), percentile(&ttfts, 95)) {
            (Some(p50), Some(p95)) => {
                format!(
                    "p50 {} · p95 {}",
                    fmt_duration_us(p50),
                    fmt_duration_us(p95)
                )
            }
            _ => "–".to_string(),
        },
        theme,
    ));
    body.push(kv(
        "Stream rate",
        &fmt_rate_label(aggregate_stream_tps(successful.iter().copied())),
        theme,
    ));
    body.push(kv(
        "E2E output rate",
        &fmt_rate_label(aggregate_e2e_tps(successful.iter().copied())),
        theme,
    ));
    let provider_decode = aggregate_decode_tps(successful.iter().copied());
    body.push(kv(
        "Server decode",
        &provider_decode.map_or_else(|| "–".to_string(), |rate| fmt_rate_label(Some(rate))),
        theme,
    ));
    body.push(kv("Timing", "client observed", theme));
    body.push(Line::from(""));

    if rounds.is_empty() {
        body.push(placeholder(
            "No model request attempts recorded yet.",
            true,
            theme.muted(),
        ));
        return (body, None);
    }

    let labels = rounds
        .iter()
        .map(|round| round_row_label(round.number))
        .collect::<Vec<_>>();
    let states = rounds
        .iter()
        .map(|round| round_state(round, theme).0.to_string())
        .collect::<Vec<_>>();
    let first = rounds
        .iter()
        .map(|round| fmt_optional_duration(round_first_ttft_us(round)))
        .collect::<Vec<_>>();
    let stream = rounds
        .iter()
        .map(|round| fmt_rate(round_stream_tps(round)))
        .collect::<Vec<_>>();
    let e2e = rounds
        .iter()
        .map(|round| fmt_rate(round_e2e_tps(round)))
        .collect::<Vec<_>>();
    let widths = table_widths(
        body_width,
        ["Round", "State", "First", "Stream", "E2E"],
        [&labels, &states, &first, &stream, &e2e],
    );
    body.push(table_line(
        ["Round", "State", "First", "Stream", "E2E"],
        widths,
        [theme.muted(); 5],
        theme.panel(),
    ));

    let mut selected_line = None;
    for (index, round) in rounds.iter().enumerate() {
        let selected_row = index == selected;
        let bg = if selected_row {
            theme.selected()
        } else {
            theme.panel()
        };
        if selected_row {
            selected_line = Some(body.len());
        }
        let (_, state_color) = round_state(round, theme);
        body.push(table_line(
            [
                labels[index].as_str(),
                states[index].as_str(),
                first[index].as_str(),
                stream[index].as_str(),
                e2e[index].as_str(),
            ],
            widths,
            [
                theme.fg(),
                state_color,
                theme.muted(),
                theme.fg(),
                theme.muted(),
            ],
            bg,
        ));
    }
    (body, selected_line)
}

fn detail_body(
    round: &PerformanceRound<'_>,
    body_width: usize,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let mut body = Vec::new();
    let completed = round
        .attempts
        .iter()
        .filter(|record| record.status == RequestUsageStatus::Completed)
        .count();
    let failed = round.attempts.len().saturating_sub(completed);
    body.push(kv(
        "First output",
        &fmt_optional_duration(round_first_ttft_us(round)),
        theme,
    ));
    body.push(kv(
        "Requests",
        &if failed == 0 {
            format!("{completed} completed")
        } else {
            format!("{completed} completed · {failed} non-success")
        },
        theme,
    ));
    body.push(kv(
        "Stream rate",
        &fmt_rate_label(round_stream_tps(round)),
        theme,
    ));
    body.push(kv(
        "E2E output rate",
        &fmt_rate_label(round_e2e_tps(round)),
        theme,
    ));
    body.push(Line::from(""));
    body.push(section_heading("Turns / attempts", theme));

    let attempts = round.attempts.iter().rev().copied().collect::<Vec<_>>();
    let labels = attempts
        .iter()
        .map(|record| attempt_label(record))
        .collect::<Vec<_>>();
    let states = attempts
        .iter()
        .map(|record| attempt_state(record, theme).0.to_string())
        .collect::<Vec<_>>();
    let ttft = attempts
        .iter()
        .map(|record| fmt_optional_duration(observed_ttft_us(record)))
        .collect::<Vec<_>>();
    let stream = attempts
        .iter()
        .map(|record| {
            fmt_rate(
                record
                    .performance
                    .and_then(|performance| performance.observed_stream_tps()),
            )
        })
        .collect::<Vec<_>>();
    let e2e = attempts
        .iter()
        .map(|record| {
            fmt_rate(
                record
                    .performance
                    .and_then(|performance| performance.e2e_output_tps(record.completion_tokens)),
            )
        })
        .collect::<Vec<_>>();
    let quality = attempts
        .iter()
        .map(|record| quality_label(record).to_string())
        .collect::<Vec<_>>();
    let widths = table_widths(
        body_width,
        ["Turn", "State", "TTFT", "Stream", "E2E", "Q"],
        [&labels, &states, &ttft, &stream, &e2e, &quality],
    );
    body.push(table_line(
        ["Turn", "State", "TTFT", "Stream", "E2E", "Q"],
        widths,
        [theme.muted(); 6],
        theme.panel(),
    ));
    for (index, record) in attempts.iter().enumerate() {
        let (_, state_color) = attempt_state(record, theme);
        body.push(table_line(
            [
                labels[index].as_str(),
                states[index].as_str(),
                ttft[index].as_str(),
                stream[index].as_str(),
                e2e[index].as_str(),
                quality[index].as_str(),
            ],
            widths,
            [
                theme.fg(),
                state_color,
                theme.muted(),
                theme.fg(),
                theme.muted(),
                theme.info(),
            ],
            theme.panel(),
        ));
        if let Some(error) = record.error.as_deref().filter(|error| !error.is_empty()) {
            body.push(Line::from(vec![
                Span::styled("  ↳ ", Style::default().fg(theme.err())),
                Span::styled(
                    truncate(error, body_width.saturating_sub(4)),
                    Style::default().fg(theme.muted()),
                ),
            ]));
        }
    }
    body.push(Line::from(""));
    body.push(Line::from(Span::styled(
        "Q: A provider decode · B reported usage · C estimated usage",
        Style::default().fg(theme.muted()),
    )));
    body.push(Line::from(Span::styled(
        "TTFT/Stream/E2E are client-observed and include network behavior.",
        Style::default().fg(theme.muted()),
    )));
    body
}

fn performance_rounds(report: &TokenSourceReport) -> Vec<PerformanceRound<'_>> {
    let mut rounds = BTreeMap::<u64, Vec<&RequestUsageRecord>>::new();
    for record in report.rows.iter().flat_map(|row| row.requests.iter()) {
        if record.key.actor_id == "master" {
            rounds.entry(record.key.round).or_default().push(record);
        }
    }
    for attempts in rounds.values_mut() {
        attempts.sort_by_key(|record| (record.key.turn, record.key.attempt));
    }
    rounds
        .into_iter()
        .rev()
        .map(|(number, attempts)| PerformanceRound { number, attempts })
        .collect()
}

fn successful<'a>(round: &'a PerformanceRound<'a>) -> impl Iterator<Item = &'a RequestUsageRecord> {
    round
        .attempts
        .iter()
        .copied()
        .filter(|record| record.status == RequestUsageStatus::Completed)
}

fn round_first_ttft_us(round: &PerformanceRound<'_>) -> Option<u64> {
    round
        .attempts
        .iter()
        .find_map(|record| observed_ttft_us(record))
}

fn observed_ttft_us(record: &RequestUsageRecord) -> Option<u64> {
    record
        .performance
        .and_then(|performance| performance.visible_ttft_us.or(performance.ttft_us))
}

fn round_stream_tps(round: &PerformanceRound<'_>) -> Option<f64> {
    aggregate_stream_tps(successful(round))
}

fn round_e2e_tps(round: &PerformanceRound<'_>) -> Option<f64> {
    aggregate_e2e_tps(successful(round))
}

fn aggregate_stream_tps<'a>(records: impl Iterator<Item = &'a RequestUsageRecord>) -> Option<f64> {
    let mut tokens = 0u64;
    let mut duration = 0u64;
    for performance in records.filter_map(|record| record.performance) {
        let Some(stream_us) = performance.stream_us else {
            continue;
        };
        let Some(stream_tokens) = performance
            .streamed_output_tokens
            .checked_sub(performance.first_output_tokens)
        else {
            continue;
        };
        if stream_us == 0 || stream_tokens == 0 || performance.output_events < 2 {
            continue;
        }
        tokens = tokens.saturating_add(stream_tokens);
        duration = duration.saturating_add(stream_us);
    }
    (tokens > 0 && duration > 0).then(|| tokens as f64 * 1_000_000.0 / duration as f64)
}

fn aggregate_e2e_tps<'a>(records: impl Iterator<Item = &'a RequestUsageRecord>) -> Option<f64> {
    let mut tokens = 0u64;
    let mut duration = 0u64;
    for record in records {
        let Some(e2e_us) = record
            .performance
            .and_then(|performance| performance.e2e_us)
        else {
            continue;
        };
        if e2e_us == 0 || record.completion_tokens <= 0 {
            continue;
        }
        tokens = tokens.saturating_add(record.completion_tokens as u64);
        duration = duration.saturating_add(e2e_us);
    }
    (tokens > 0 && duration > 0).then(|| tokens as f64 * 1_000_000.0 / duration as f64)
}

fn aggregate_decode_tps<'a>(records: impl Iterator<Item = &'a RequestUsageRecord>) -> Option<f64> {
    let mut tokens = 0u64;
    let mut duration = 0u64;
    for performance in records.filter_map(|record| record.performance) {
        let (Some(decode_us), Some(output_tokens)) = (
            performance.provider_decode_us,
            performance.provider_output_tokens,
        ) else {
            continue;
        };
        let Some(decode_tokens) = output_tokens.checked_sub(1) else {
            continue;
        };
        if decode_us == 0 || decode_tokens == 0 {
            continue;
        }
        tokens = tokens.saturating_add(decode_tokens);
        duration = duration.saturating_add(decode_us);
    }
    (tokens > 0 && duration > 0).then(|| tokens as f64 * 1_000_000.0 / duration as f64)
}

fn percentile(sorted: &[u64], percentile: usize) -> Option<u64> {
    if sorted.is_empty() {
        return None;
    }
    let index = (sorted.len().saturating_sub(1) * percentile).div_ceil(100);
    sorted.get(index).copied()
}

fn quality_label(record: &RequestUsageRecord) -> &'static str {
    if record
        .performance
        .is_some_and(|performance| performance.provider_decode_tps().is_some())
    {
        "A"
    } else if record.source == RequestUsageSource::Reported {
        "B"
    } else if record.source == RequestUsageSource::Estimated {
        "C"
    } else {
        "–"
    }
}

fn round_state(round: &PerformanceRound<'_>, theme: &Theme) -> (&'static str, mutx_engine::Color) {
    let statuses = round.attempts.iter().map(|record| record.status);
    let mut in_flight = false;
    let mut failed = false;
    let mut interrupted = false;
    let mut completed = false;
    for status in statuses {
        match status {
            RequestUsageStatus::InFlight => in_flight = true,
            RequestUsageStatus::Failed => failed = true,
            RequestUsageStatus::Interrupted | RequestUsageStatus::Abandoned => interrupted = true,
            RequestUsageStatus::Completed => completed = true,
        }
    }
    if in_flight {
        ("in flight", theme.info())
    } else if failed {
        ("failed", theme.err())
    } else if interrupted {
        ("interrupted", theme.warn())
    } else if completed {
        ("done", theme.ok())
    } else {
        ("abandoned", theme.warn())
    }
}

fn attempt_state(record: &RequestUsageRecord, theme: &Theme) -> (&'static str, mutx_engine::Color) {
    match record.status {
        RequestUsageStatus::InFlight => ("in flight", theme.info()),
        RequestUsageStatus::Completed => ("completed", theme.ok()),
        RequestUsageStatus::Interrupted => ("interrupted", theme.warn()),
        RequestUsageStatus::Failed => ("failed", theme.err()),
        RequestUsageStatus::Abandoned => ("abandoned", theme.warn()),
    }
}

fn table_widths<const N: usize>(
    body_width: usize,
    headers: [&str; N],
    columns: [&Vec<String>; N],
) -> [usize; N] {
    let mut widths = std::array::from_fn(|index| {
        columns[index]
            .iter()
            .map(|cell| cell.width())
            .chain(std::iter::once(headers[index].width()))
            .max()
            .unwrap_or(1)
    });
    let fixed_gaps = 2 * N.saturating_sub(1);
    while widths.iter().sum::<usize>() + fixed_gaps > body_width {
        let Some((index, _)) = widths.iter().enumerate().max_by_key(|(_, width)| **width) else {
            break;
        };
        if widths[index] <= 4 {
            break;
        }
        widths[index] -= 1;
    }
    widths
}

fn table_line<const N: usize>(
    cells: [&str; N],
    widths: [usize; N],
    colors: [mutx_engine::Color; N],
    bg: mutx_engine::Color,
) -> Line<'static> {
    let mut spans = Vec::with_capacity(N * 2);
    for index in 0..N {
        if index > 0 {
            spans.push(Span::styled("  ", Style::default().bg(bg)));
        }
        let cell = truncate(cells[index], widths[index]);
        let rendered = if index < 2 {
            format!("{cell:<width$}", width = widths[index])
        } else {
            format!("{cell:>width$}", width = widths[index])
        };
        spans.push(Span::styled(
            rendered,
            Style::default().fg(colors[index]).bg(bg),
        ));
    }
    Line::from(spans)
}

fn kv(key: &str, value: &str, theme: &Theme) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{key:<22}"), Style::default().fg(theme.muted())),
        Span::styled(value.to_string(), Style::default().fg(theme.fg())),
    ])
}

fn section_heading(text: &str, theme: &Theme) -> Line<'static> {
    Line::from(Span::styled(
        text.to_string(),
        Style::default()
            .fg(theme.brand())
            .add_modifier(Modifier::BOLD),
    ))
}

fn fmt_rate(rate: Option<f64>) -> String {
    rate.filter(|rate| rate.is_finite() && *rate > 0.0)
        .map_or_else(
            || "–".to_string(),
            |rate| {
                if rate < 10.0 {
                    format!("{rate:.1}")
                } else {
                    format!("{rate:.0}")
                }
            },
        )
}

fn fmt_rate_label(rate: Option<f64>) -> String {
    rate.filter(|rate| rate.is_finite() && *rate > 0.0)
        .map_or_else(|| "–".to_string(), |rate| format!("{rate:.1} tok/s"))
}

fn fmt_optional_duration(duration_us: Option<u64>) -> String {
    duration_us.map_or_else(|| "–".to_string(), fmt_duration_us)
}

fn fmt_duration_us(duration_us: u64) -> String {
    if duration_us < 1_000 {
        format!("{duration_us}µs")
    } else if duration_us < 1_000_000 {
        format!("{:.0}ms", duration_us as f64 / 1_000.0)
    } else if duration_us < 10_000_000 {
        format!("{:.2}s", duration_us as f64 / 1_000_000.0)
    } else {
        format!("{:.1}s", duration_us as f64 / 1_000_000.0)
    }
}

fn round_label(number: u64) -> String {
    if number == 0 {
        "Earlier performance".to_string()
    } else {
        format!("{} round", ordinal(number))
    }
}

fn round_row_label(number: u64) -> String {
    if number == 0 {
        "Earlier".to_string()
    } else {
        ordinal(number)
    }
}

fn attempt_label(record: &RequestUsageRecord) -> String {
    if record.key.attempt > 1 {
        format!(
            "{} - {}",
            ordinal(record.key.turn as u64),
            ordinal(record.key.attempt as u64)
        )
    } else {
        ordinal(record.key.turn as u64)
    }
}

fn ordinal(number: u64) -> String {
    let suffix = if number % 100 / 10 == 1 {
        "th"
    } else {
        match number % 10 {
            1 => "st",
            2 => "nd",
            3 => "rd",
            _ => "th",
        }
    };
    format!("{number}{suffix}")
}

fn truncate(text: &str, width: usize) -> String {
    if text.width() <= width {
        return text.to_string();
    }
    if width == 0 {
        return String::new();
    }
    let mut out = String::new();
    let keep = width.saturating_sub(1);
    for character in text.chars() {
        let next = character.width().unwrap_or(0);
        if out.width() + next > keep {
            break;
        }
        out.push(character);
    }
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use muta_contracts::{
        PerformanceTimingSource, RequestPerformance, RequestUsageKey, StreamTokenSource,
    };

    fn record(round: u64, turn: u32, ttft_us: u64, stream_us: u64) -> RequestUsageRecord {
        RequestUsageRecord {
            key: RequestUsageKey {
                session_id: "session".to_string(),
                actor_id: "master".to_string(),
                round,
                turn,
                attempt: 1,
            },
            status: RequestUsageStatus::Completed,
            source: RequestUsageSource::Reported,
            completion_tokens: 101,
            performance: Some(RequestPerformance {
                ttft_us: Some(ttft_us),
                visible_ttft_us: Some(ttft_us),
                stream_us: Some(stream_us),
                e2e_us: Some(ttft_us + stream_us),
                streamed_output_tokens: 101,
                first_output_tokens: 1,
                output_events: 101,
                timing_source: PerformanceTimingSource::ClientObserved,
                stream_token_source: StreamTokenSource::Cl100k,
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn aggregation_separates_ttft_from_stream_rate() {
        let records = [
            record(1, 1, 500_000, 1_000_000),
            record(2, 1, 2_000_000, 1_000_000),
        ];
        let stream = aggregate_stream_tps(records.iter()).expect("stream rate");
        assert!((stream - 100.0).abs() < 0.001);
        let e2e = aggregate_e2e_tps(records.iter()).expect("e2e rate");
        assert!(e2e < stream, "TTFT must affect E2E but not stream TPS");
    }

    #[test]
    fn single_event_has_no_stream_rate() {
        let mut sample = record(1, 1, 10, 10);
        sample
            .performance
            .as_mut()
            .expect("performance")
            .output_events = 1;
        assert_eq!(aggregate_stream_tps([&sample].into_iter()), None);
    }
}
