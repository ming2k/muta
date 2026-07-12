//! Token-source report modal — an itemized "bill" of token usage per
//! provider+model, with a per-model drill-in showing the upstream-vs-estimated
//! split, every round's line items, and Anthropic prompt-cache efficiency.
//!
//! Opened by clicking the context meter in the hint bar. ↑/↓ select a line,
//! Enter opens its detail, Esc backs out / closes. The data is a live snapshot
//! of the shared `TokenSourceLedger`, so it reflects every turn booked so far
//! this session.

use neenee_core::{
    ContextTokenSnapshot, ContextTokenSource, RequestUsageSource, RequestUsageStatus,
    TokenSourceReport,
};
use neenee_tui::{
    Color, Frame, Modifier, Style, {Line, Span},
};
use unicode_width::UnicodeWidthStr;

use super::common::placeholder;
use crate::render::Theme;
use crate::render::design::MODAL_INNER_H_PADDING;
use crate::render::primitives::{
    ContentModalSpec, FooterHint, content_modal_area, content_modal_probe, modal_chrome_rows,
    modal_frame, modal_header, render_body, render_modal_footer,
};

/// Live context-meter values shown above the completed-request ledger.
#[derive(Debug, Clone, Copy)]
pub struct ContextUsageView {
    pub snapshot: Option<ContextTokenSnapshot>,
    pub window_tokens: usize,
}

/// Draw the token bill (list) or, when `detail` is set, the per-model breakdown
/// for `report.rows[selected]`. `selected` is the highlighted line in the bill;
/// `scroll` drives the detail body. Returns the painted panel rect.
pub fn draw_token_report_modal(
    frame: &mut Frame,
    report: &TokenSourceReport,
    context: ContextUsageView,
    selected: usize,
    detail: bool,
    scroll: &mut usize,
    theme: &Theme,
) -> neenee_tui::Rect {
    // Probe the content width so column layout adapts to the terminal.
    let geometry = ContentModalSpec::TOKEN_REPORT;
    let probe = content_modal_probe(frame, geometry);
    let body_width = (probe.width as usize)
        .saturating_sub(2 * MODAL_INNER_H_PADDING as usize)
        .max(1);

    let drill = detail && !report.rows.is_empty();
    let sel = selected.min(report.rows.len().saturating_sub(1));

    let (title, body, footer): (&str, Vec<Line>, Vec<FooterHint>) = if drill {
        (
            "Token Detail",
            detail_body(report, sel, body_width, theme),
            vec![
                FooterHint::always("↑↓", "scroll"),
                FooterHint::always("Esc", "back"),
            ],
        )
    } else if report.rows.is_empty() {
        (
            "Context Usage",
            list_body(
                report,
                context.snapshot,
                context.window_tokens,
                sel,
                body_width,
                theme,
            ),
            vec![FooterHint::always("Esc", "close")],
        )
    } else {
        (
            "Context Usage",
            list_body(
                report,
                context.snapshot,
                context.window_tokens,
                sel,
                body_width,
                theme,
            ),
            vec![
                FooterHint::always("↑↓", "select"),
                FooterHint::always("Enter", "details"),
                FooterHint::always("Esc", "close"),
            ],
        )
    };

    // ── Size the panel to the content and paint it ──
    let desired = body.len() as u16 + modal_chrome_rows(geometry.modal_spec());
    let area = content_modal_area(frame, geometry, desired);
    let f = modal_frame(frame, area, theme.panel(), true, true);

    modal_header(frame, f.header, title, theme);

    render_body(frame, f.body, body, scroll, None, 0, false, theme);

    if let Some(fo) = f.footer {
        render_modal_footer(frame, fo, &footer, theme);
    }
    area
}

/// The bill: one selectable line per provider+model, plus a grand total.
fn list_body<'a>(
    report: &TokenSourceReport,
    current_context: Option<ContextTokenSnapshot>,
    context_window: usize,
    sel: usize,
    body_width: usize,
    theme: &Theme,
) -> Vec<Line<'a>> {
    let mut body: Vec<Line> = Vec::new();

    body.push(Line::from(Span::styled(
        "Current AI-visible context",
        Style::default()
            .fg(theme.brand())
            .add_modifier(Modifier::BOLD),
    )));
    if let Some(snapshot) = current_context {
        let size = if context_window > 0 {
            let ratio = (snapshot.tokens as f64 / context_window as f64).clamp(0.0, 1.0);
            format!(
                "{} / {}  ({}%)",
                fmt_token_count(snapshot.tokens),
                fmt_token_count(context_window),
                (ratio * 100.0).round() as u32,
            )
        } else {
            fmt_token_count(snapshot.tokens)
        };
        let (source, source_color) = match snapshot.source {
            ContextTokenSource::Api => ("provider usage (reported)", theme.ok()),
            ContextTokenSource::Projection => {
                ("local request projection (estimated)", theme.warn())
            }
        };
        body.push(kv("Size", &size, theme.fg(), theme));
        body.push(kv("Source", source, source_color, theme));
    } else {
        body.push(placeholder(
            "Current context estimate unavailable.",
            true,
            theme.muted(),
        ));
    }
    body.push(Line::from(""));
    body.push(Line::from(Span::styled(
        "Request usage",
        Style::default()
            .fg(theme.brand())
            .add_modifier(Modifier::BOLD),
    )));

    if report.rows.is_empty() {
        body.push(placeholder(
            "No model request attempts recorded yet.",
            true,
            theme.muted(),
        ));
        if current_context.is_some_and(|snapshot| snapshot.source == ContextTokenSource::Projection)
        {
            body.push(Line::from(Span::styled(
                "The context above is the initial pre-request estimate.",
                Style::default().fg(theme.muted()),
            )));
        }
        return body;
    }

    const TOTAL_W: usize = 12;
    const SRC_W: usize = 11;
    // 2 leading marker cols + 2 single-space gaps.
    let name_budget = body_width.saturating_sub(TOTAL_W + SRC_W + 4).max(12);

    // Header row.
    body.push(Line::from(vec![
        Span::styled(
            format!("  {:<w$}", "Provider / Model", w = name_budget),
            Style::default().fg(theme.muted()),
        ),
        Span::styled(
            format!("{:>w$}", "Tokens", w = TOTAL_W),
            Style::default().fg(theme.muted()),
        ),
        Span::styled(
            format!(" {:>w$}", "Source", w = SRC_W),
            Style::default().fg(theme.muted()),
        ),
    ]));
    body.push(rule(body_width, theme));

    // One selectable line per provider+model.
    for (i, row) in report.rows.iter().enumerate() {
        let selected = i == sel;
        let marker = if selected { "> " } else { "  " };
        let label = truncate_str(&format!("{} · {}", row.provider, row.model), name_budget);
        let (src_text, src_color) = if row.totals.total() == 0
            && row
                .requests
                .iter()
                .any(|request| request.status == RequestUsageStatus::InFlight)
        {
            ("in flight".to_string(), theme.info())
        } else {
            source_label(
                row.totals.reported_tokens,
                row.totals.estimated_tokens,
                theme,
            )
        };
        let name_style = if selected {
            Style::default()
                .fg(theme.brand())
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.fg())
        };
        body.push(Line::from(vec![
            Span::styled(
                format!("{marker}{:<w$}", label, w = name_budget),
                name_style,
            ),
            Span::styled(
                format!("{:>w$}", fmt_tokens(row.totals.total()), w = TOTAL_W),
                Style::default().fg(theme.fg()),
            ),
            Span::styled(
                format!(" {:>w$}", src_text, w = SRC_W),
                Style::default().fg(src_color),
            ),
        ]));
    }

    // Grand-total line.
    body.push(rule(body_width, theme));
    body.push(Line::from(vec![
        Span::styled(
            format!("  {:<w$}", "Total", w = name_budget),
            Style::default().fg(theme.fg()).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(
                "{:>w$}",
                fmt_tokens(report.grand_total.total()),
                w = TOTAL_W
            ),
            Style::default()
                .fg(theme.brand())
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!(" {:>w$}", "", w = SRC_W), Style::default()),
    ]));

    body.push(Line::from(""));
    body.push(Line::from(Span::styled(
        "Source: % real = share of tokens from the provider's usage object (vs. local estimate).",
        Style::default().fg(theme.muted()),
    )));
    body.push(Line::from(Span::styled(
        "Select a line and press Enter to see its rounds and cache efficiency.",
        Style::default().fg(theme.muted()),
    )));
    body
}

/// The drill-in for one provider+model: source split, cache efficiency, and a
/// per-round line-item table.
fn detail_body<'a>(
    report: &TokenSourceReport,
    sel: usize,
    body_width: usize,
    theme: &Theme,
) -> Vec<Line<'a>> {
    let row = &report.rows[sel];
    let t = &row.totals;
    let mut body: Vec<Line> = Vec::new();

    body.push(Line::from(Span::styled(
        format!("{} · {}", row.provider, row.model),
        Style::default()
            .fg(theme.brand())
            .add_modifier(Modifier::BOLD),
    )));
    body.push(Line::from(""));

    let total = t.total().max(1);
    let pct_real = (t.reported_tokens as f64 / total as f64 * 100.0).round() as i64;
    body.push(kv(
        "Reported (upstream)",
        &format!("{}  ({pct_real}% of total)", fmt_tokens(t.reported_tokens)),
        theme.ok(),
        theme,
    ));
    body.push(kv(
        "Estimated (local)",
        &fmt_tokens(t.estimated_tokens),
        if t.estimated_tokens > 0 {
            theme.warn()
        } else {
            theme.muted()
        },
        theme,
    ));
    body.push(kv(
        "Reported input/output",
        &format!(
            "{} / {}",
            fmt_tokens(t.prompt_tokens),
            fmt_tokens(t.completion_tokens)
        ),
        theme.fg(),
        theme,
    ));

    if t.cache_read_tokens > 0 || t.cache_write_tokens > 0 {
        // Hit-rate = cache-read / (cache-read + reported uncached input). The
        // uncached input is the reported total minus the two cache portions.
        let uncached = (t.reported_tokens - t.cache_read_tokens - t.cache_write_tokens).max(0);
        let denom = (t.cache_read_tokens + uncached).max(1) as f64;
        let hit = (t.cache_read_tokens as f64 / denom * 100.0).round() as i64;
        body.push(kv(
            "Cache read / write",
            &format!(
                "{} / {}",
                fmt_tokens(t.cache_read_tokens),
                fmt_tokens(t.cache_write_tokens)
            ),
            theme.ok(),
            theme,
        ));
        body.push(kv(
            "Cache hit-rate",
            &format!("{hit}%  (cache_control breakpoints landing)"),
            if hit >= 50 { theme.ok() } else { theme.muted() },
            theme,
        ));
    }

    body.push(Line::from(""));
    body.push(Line::from(Span::styled(
        "Request attempts",
        Style::default()
            .fg(theme.brand())
            .add_modifier(Modifier::BOLD),
    )));
    body.push(rule(body_width, theme));
    body.push(Line::from(Span::styled(
        format!(
            "{:<13}{:<12}{:<10}{:>9}{:>9}{:>9}",
            "Request", "State", "Source", "Input", "Output", "Total"
        ),
        Style::default().fg(theme.muted()),
    )));

    if row.requests.is_empty() && row.rounds.is_empty() {
        body.push(placeholder("No per-round detail.", true, theme.muted()));
    }
    let attempts = if row.requests.is_empty() {
        row.rounds
            .iter()
            .enumerate()
            .map(|(index, round)| {
                let source = if round.reported {
                    RequestUsageSource::Reported
                } else {
                    RequestUsageSource::Estimated
                };
                (
                    "principal".to_string(),
                    round.turn,
                    if round.round == 0 {
                        index.saturating_add(1) as u32
                    } else {
                        round.round
                    },
                    1,
                    RequestUsageStatus::Completed,
                    source,
                    round.prompt_tokens,
                    round.completion_tokens,
                    round.total_tokens,
                )
            })
            .collect::<Vec<_>>()
    } else {
        row.requests
            .iter()
            .map(|request| {
                (
                    request.key.actor_id.clone(),
                    request.key.round,
                    request.key.turn,
                    request.key.attempt,
                    request.status,
                    request.source,
                    request.prompt_tokens,
                    request.completion_tokens,
                    request.total_tokens,
                )
            })
            .collect::<Vec<_>>()
    };
    let mut current_round: Option<u64> = None;
    for (actor, round, turn, attempt, status, source, prompt, completion, total) in attempts {
        if round != 0 && current_round != Some(round) {
            current_round = Some(round);
            body.push(Line::from(""));
            body.push(Line::from(Span::styled(
                format!("Round {round}"),
                Style::default()
                    .fg(theme.brand())
                    .add_modifier(Modifier::BOLD),
            )));
        }
        let (src, src_color) = match source {
            RequestUsageSource::Reported => ("reported", theme.ok()),
            RequestUsageSource::Estimated => ("estimated", theme.warn()),
            RequestUsageSource::Unknown => ("pending", theme.info()),
        };
        let (input, output) = if source == RequestUsageSource::Unknown {
            ("—".to_string(), "—".to_string())
        } else {
            (fmt_tokens(prompt), fmt_tokens(completion))
        };
        let (state, state_color) = match status {
            RequestUsageStatus::InFlight => ("in flight", theme.info()),
            RequestUsageStatus::Completed => ("completed", theme.ok()),
            RequestUsageStatus::Interrupted => ("interrupted", theme.warn()),
            RequestUsageStatus::Failed => ("failed", theme.err()),
            RequestUsageStatus::Abandoned => ("abandoned", theme.warn()),
        };
        let request_label = if actor == "principal" {
            format!("T{turn}/A{attempt}")
        } else {
            format!("E T{turn}/A{attempt}")
        };
        body.push(Line::from(vec![
            Span::styled(
                format!("{request_label:<13}"),
                Style::default().fg(theme.muted()),
            ),
            Span::styled(format!("{state:<12}"), Style::default().fg(state_color)),
            Span::styled(format!("{src:<10}"), Style::default().fg(src_color)),
            Span::styled(
                format!("{input:>9}{output:>9}{:>9}", fmt_tokens(total)),
                Style::default().fg(theme.fg()),
            ),
        ]));
    }

    body.push(Line::from(""));
    body.push(Line::from(Span::styled(
        "Reported = provider usage; estimated = local prompt + observed completion.",
        Style::default().fg(theme.muted()),
    )));
    body
}

/// A full-width horizontal rule line.
fn rule<'a>(w: usize, theme: &Theme) -> Line<'a> {
    Line::from(Span::styled(
        "─".repeat(w),
        Style::default().fg(theme.muted()),
    ))
}

/// A muted `key` + colored `value` line for the detail summary.
fn kv<'a>(k: &str, v: &str, vcolor: Color, theme: &Theme) -> Line<'a> {
    Line::from(vec![
        Span::styled(format!("{:<22}", k), Style::default().fg(theme.muted())),
        Span::styled(v.to_string(), Style::default().fg(vcolor)),
    ])
}

/// The "Source" cell for a bill line: how much of this row is authoritative.
fn source_label(reported: i64, estimated: i64, theme: &Theme) -> (String, Color) {
    let total = (reported + estimated).max(1);
    if reported > 0 && estimated == 0 {
        ("100% real".to_string(), theme.ok())
    } else if reported > 0 {
        let pct = (reported as f64 / total as f64 * 100.0).round() as i64;
        (format!("{pct}% real"), theme.warn())
    } else {
        ("estimated".to_string(), theme.muted())
    }
}

/// Format a token count with a `k`/`M` suffix for compactness in narrow columns.
fn fmt_tokens(n: i64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

/// Format a non-negative context size with the same SI suffixes used by the
/// always-visible context meter.
fn fmt_token_count(n: usize) -> String {
    if n >= 1_000_000_000 {
        format!("{:.1}B", n as f64 / 1_000_000_000.0)
    } else if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

/// Truncate a string to fit a column width, appending an ellipsis when cut.
fn truncate_str(s: &str, max: usize) -> String {
    if s.width() <= max {
        s.to_string()
    } else if max <= 1 {
        "…".to_string()
    } else {
        let mut out = s.chars().take(max.saturating_sub(1)).collect::<String>();
        out.push('…');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body_text(lines: &[Line<'_>]) -> String {
        lines
            .iter()
            .flat_map(|line| line.spans.iter().map(|span| span.content.as_ref()))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn empty_ledger_still_shows_initial_context_projection() {
        let theme = Theme::default();
        let report = TokenSourceReport::default();
        let body = list_body(
            &report,
            Some(ContextTokenSnapshot {
                tokens: 12_500,
                source: ContextTokenSource::Projection,
            }),
            200_000,
            0,
            80,
            &theme,
        );
        let text = body_text(&body);

        assert!(text.contains("Current AI-visible context"));
        assert!(text.contains("12.5k / 200.0k  (6%)"));
        assert!(text.contains("local request projection (estimated)"));
        assert!(text.contains("initial pre-request estimate"));
        assert!(!text.contains("No token usage recorded yet"));
    }

    #[test]
    fn detail_groups_lifecycle_attempts_by_turn_round_and_attempt() {
        let theme = Theme::default();
        let ledger = neenee_core::TokenSourceLedger::new();
        let first = ledger.begin_request("session", "relay", "model", 2, 1, 800);
        ledger.settle_request(&first, RequestUsageStatus::Interrupted, None, 20);
        let retry = ledger.begin_request("session", "relay", "model", 2, 1, 800);
        ledger.settle_request(&retry, RequestUsageStatus::Completed, None, 40);
        let report = ledger.snapshot_for_session("session");

        let text = body_text(&detail_body(&report, 0, 80, &theme));
        assert!(text.contains("Round 2"));
        assert!(text.contains("T1/A1"));
        assert!(text.contains("T1/A2"));
        assert!(text.contains("interrupted"));
        assert!(text.contains("completed"));
        assert!(text.contains("estimated"));
    }
}
