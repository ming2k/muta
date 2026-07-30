//! Context-usage modal: current AI-visible context plus request usage grouped
//! by user round. Opening a round reveals the model turns inside it; provider
//! and model remain ledger metadata rather than the report's navigation axis.
//!
//! Opened by clicking the context meter in the hint bar. Up and down select a
//! round, Enter opens its turns, and Esc backs out or closes. Values use a
//! calm, single-foreground palette; only turn lifecycle state is colored.

use std::collections::BTreeMap;

use neenee_core::{
    ContextTokenSnapshot, RequestUsageRecord, RequestUsageSource, RequestUsageStatus,
    TokenSourceReport, TokenTurn,
};
use neenee_tui_engine::{
    Frame, Modifier, Style, {Line, Span},
};
use unicode_width::UnicodeWidthStr;

use super::common::placeholder;
use crate::tui::design::MODAL_INNER_H_PADDING;
use crate::tui::primitives::{
    ContentModalSpec, FooterHint, SCROLL_EDGE_MARGIN, content_modal_area, content_modal_probe,
    keyvocab, modal_chrome_rows, modal_frame, modal_header, render_body, render_modal_footer,
};
use crate::tui::view::Theme;

/// Live context-meter values shown above the completed-request ledger.
#[derive(Debug, Clone, Copy)]
pub struct ContextUsageView {
    pub snapshot: Option<ContextTokenSnapshot>,
    pub window_tokens: usize,
    /// Latest per-round throughput summary, surfaced as an honest tokens/sec
    /// that excludes the time the round spent parked on human decisions.
    /// `None` until the first natural round completes.
    pub round_summary: Option<neenee_core::RoundSummary>,
}

/// Number of user rounds represented by a report.
///
/// Kept beside the renderer so shell navigation and rendered grouping always
/// use the same definition of a round.
pub fn token_report_round_count(report: &TokenSourceReport) -> usize {
    usage_rounds(report).len()
}

/// Draw the round list or, when `detail` is set, the turn breakdown for the
/// selected round. `scroll` drives the visible body and is clamped by the shared
/// modal renderer. Returns the painted panel rectangle.
pub fn draw_token_report_modal(
    frame: &mut Frame,
    report: &TokenSourceReport,
    context: ContextUsageView,
    selected: usize,
    detail: bool,
    scroll: &mut usize,
    theme: &Theme,
) -> neenee_tui_engine::Rect {
    let geometry = ContentModalSpec::TOKEN_REPORT;
    let probe = content_modal_probe(frame, geometry);
    let body_width = (probe.width as usize)
        .saturating_sub(2 * MODAL_INNER_H_PADDING as usize)
        .max(1);

    let round_count = token_report_round_count(report);
    let drill = detail && round_count > 0;
    let selected = selected.min(round_count.saturating_sub(1));

    let (title, body, footer): (&str, Vec<Line>, Vec<FooterHint>) = if drill {
        (
            "Round Usage",
            detail_body(report, selected, body_width, theme),
            vec![
                FooterHint::always(keyvocab::ARROWS_UD, "scroll"),
                FooterHint::always(keyvocab::ESC, "rounds"),
            ],
        )
    } else if round_count == 0 {
        (
            "Context Usage",
            list_body(
                report,
                context.snapshot,
                context.window_tokens,
                context.round_summary,
                selected,
                body_width,
                theme,
            ),
            vec![FooterHint::always(keyvocab::ESC, "close")],
        )
    } else {
        (
            "Context Usage",
            list_body(
                report,
                context.snapshot,
                context.window_tokens,
                context.round_summary,
                selected,
                body_width,
                theme,
            ),
            vec![
                FooterHint::always(keyvocab::ARROWS_UD, "select"),
                FooterHint::always(keyvocab::ENTER, "turns"),
                FooterHint::always(keyvocab::ESC, "close"),
            ],
        )
    };

    let follow = if drill {
        None
    } else {
        body.iter().position(|line| {
            line.spans
                .first()
                .is_some_and(|span| span.content.starts_with("> "))
        })
    };
    let desired = body.len() as u16 + modal_chrome_rows(geometry.modal_spec());
    let area = content_modal_area(frame, geometry, desired);
    let modal = modal_frame(frame, area, theme.panel(), true, true);

    modal_header(frame, modal.header, title, theme);
    render_body(
        frame,
        modal.body,
        body,
        scroll,
        follow,
        if follow.is_some() {
            SCROLL_EDGE_MARGIN
        } else {
            0
        },
        false,
        theme,
    );

    if let Some(footer_area) = modal.footer {
        render_modal_footer(frame, footer_area, &footer, theme);
    }
    area
}

/// Top level: one selectable row per user round.
fn list_body(
    report: &TokenSourceReport,
    current_context: Option<ContextTokenSnapshot>,
    context_window: usize,
    latest_tps: Option<neenee_core::RoundSummary>,
    selected: usize,
    body_width: usize,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let rounds = usage_rounds(report);
    let mut body = Vec::new();

    body.push(section_heading("Current AI-visible context", theme));
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
        body.push(kv_styled(
            "Size",
            &size,
            Style::default().fg(theme.fg()),
            theme,
        ));
    } else {
        body.push(placeholder(
            "Current context estimate unavailable.",
            true,
            theme.muted(),
        ));
    }

    // Latest generation throughput: output tokens / *active* generation time.
    // Active time excludes the human-decision pause (permission prompts /
    // ask_user), so this reflects the server's real efficiency, not how long
    // the user deliberated. The pause share is shown parenthetically so a
    // round that was mostly waiting is not misread as a slow model.
    if let Some(summary) = latest_tps {
        let tps = summary.tps();
        let tps_label = if tps > 0.0 {
            format!("{:.1} tok/s", tps)
        } else {
            "–".to_string()
        };
        let active_s = summary.active_ms() as f64 / 1000.0;
        let paused_pct = if summary.duration_ms > 0 {
            ((summary.paused_ms as f64 / summary.duration_ms as f64) * 100.0).round() as u32
        } else {
            0
        };
        let detail = format!(
            "{tps_label}  ·  {active_s:.1}s active  ·  {paused_pct}% paused  ·  round {}",
            summary.round
        );
        body.push(kv_styled(
            "Throughput",
            &detail,
            Style::default().fg(theme.fg()),
            theme,
        ));
    }

    body.push(Line::from(""));
    body.push(section_heading("Request usage", theme));

    if rounds.is_empty() {
        body.push(placeholder(
            "No model request attempts recorded yet.",
            true,
            theme.muted(),
        ));
        return body;
    }

    const TOKENS_W: usize = 12;
    const TURNS_W: usize = 9;
    let label_width = body_width.saturating_sub(TOKENS_W + TURNS_W + 3).max(10);

    body.push(Line::from(vec![
        Span::styled(
            format!("  {:<width$}", "Round", width = label_width),
            Style::default().fg(theme.muted()),
        ),
        Span::styled(
            format!("{:>width$}", "Tokens", width = TOKENS_W),
            Style::default().fg(theme.muted()),
        ),
        Span::styled(
            format!(" {:>width$}", "Turns", width = TURNS_W),
            Style::default().fg(theme.muted()),
        ),
    ]));
    body.push(rule(body_width, theme));

    for (index, round) in rounds.iter().enumerate() {
        let is_selected = index == selected;
        let marker = if is_selected { "> " } else { "  " };
        let label = truncate_str(&round_label(round.number), label_width);
        let label_style = if is_selected {
            Style::default()
                .fg(theme.brand())
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.fg())
        };
        let token_text = if round.totals.has_tokens() {
            fmt_tokens(round.totals.total())
        } else {
            "—".to_string()
        };
        let turn_text = if round.totals.pending {
            format!("{} …", round.turns.len())
        } else {
            format!("{} ›", round.turns.len())
        };

        body.push(Line::from(vec![
            Span::styled(
                format!("{marker}{label:<width$}", width = label_width),
                label_style,
            ),
            Span::styled(
                format!("{token_text:>width$}", width = TOKENS_W),
                Style::default().fg(theme.fg()),
            ),
            Span::styled(
                format!(" {turn_text:>width$}", width = TURNS_W),
                if round.totals.pending {
                    Style::default().fg(theme.info())
                } else {
                    Style::default().fg(theme.muted())
                },
            ),
        ]));
    }

    body.push(rule(body_width, theme));
    body.push(Line::from(vec![
        Span::styled(
            format!("  {:<width$}", "Total", width = label_width),
            Style::default().fg(theme.fg()).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(
                "{:>width$}",
                fmt_tokens(report.grand_total.total()),
                width = TOKENS_W
            ),
            Style::default()
                .fg(theme.fg())
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(" {:>width$}", "", width = TURNS_W),
            Style::default(),
        ),
    ]));

    body.push(Line::from(""));
    body.push(Line::from(Span::styled(
        "Enter a round to inspect its model turns.",
        Style::default().fg(theme.muted()),
    )));
    body
}

/// Second level: aggregate all request attempts into the model turns of one
/// selected user round.
fn detail_body(
    report: &TokenSourceReport,
    selected: usize,
    body_width: usize,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let rounds = usage_rounds(report);
    let round = &rounds[selected];
    let mut body = Vec::new();

    body.push(section_heading(&round_label(round.number), theme));
    body.push(Line::from(""));
    body.push(kv_styled(
        "Total",
        &fmt_tokens(round.totals.total()),
        Style::default().fg(theme.fg()),
        theme,
    ));
    let attempt_count = round
        .turns
        .values()
        .map(|turn| turn.attempt_count)
        .sum::<usize>();
    body.push(kv_styled(
        "Turns / attempts",
        &format!("{} / {attempt_count}", round.turns.len()),
        Style::default().fg(theme.fg()),
        theme,
    ));
    if round.totals.known_split {
        body.push(kv_styled(
            "Input / output",
            &format!(
                "{} / {}",
                fmt_tokens(round.totals.prompt_tokens),
                fmt_tokens(round.totals.completion_tokens)
            ),
            Style::default().fg(theme.fg()),
            theme,
        ));
    }

    if round.totals.cache_read_tokens > 0 || round.totals.cache_write_tokens > 0 {
        let uncached = (round.totals.reported_tokens
            - round.totals.cache_read_tokens
            - round.totals.cache_write_tokens)
            .max(0);
        let denominator = (round.totals.cache_read_tokens + uncached).max(1) as f64;
        let hit_rate = (round.totals.cache_read_tokens as f64 / denominator * 100.0).round() as i64;
        body.push(kv_styled(
            "Cache read / write",
            &format!(
                "{} / {}",
                fmt_tokens(round.totals.cache_read_tokens),
                fmt_tokens(round.totals.cache_write_tokens)
            ),
            Style::default().fg(theme.fg()),
            theme,
        ));
        body.push(kv_styled(
            "Cache hit rate",
            &format!("{hit_rate}%"),
            Style::default().fg(theme.fg()),
            theme,
        ));
    }

    body.push(Line::from(""));
    body.push(section_heading("Turns", theme));
    body.push(rule(body_width, theme));

    let full_table = body_width >= 62;
    if full_table {
        const STATE_W: usize = 16;
        const VALUE_W: usize = 10;
        let round_width = body_width.saturating_sub(STATE_W + VALUE_W * 3).max(10);
        body.push(Line::from(Span::styled(
            format!(
                "{:<round_width$}{:<STATE_W$}{:>VALUE_W$}{:>VALUE_W$}{:>VALUE_W$}",
                "Turn", "State", "Input", "Output", "Total"
            ),
            Style::default().fg(theme.muted()),
        )));

        for turn in round.turns.values() {
            let label = truncate_str(&turn_label(turn), round_width);
            let (state, state_style) = turn_state(turn, theme);
            let (input, output) = if turn.totals.known_split {
                (
                    fmt_tokens(turn.totals.prompt_tokens),
                    fmt_tokens(turn.totals.completion_tokens),
                )
            } else {
                ("—".to_string(), "—".to_string())
            };
            let total = if turn.totals.has_tokens() {
                fmt_tokens(turn.totals.total())
            } else {
                "—".to_string()
            };
            body.push(Line::from(vec![
                Span::styled(
                    format!("{label:<round_width$}"),
                    Style::default().fg(theme.fg()),
                ),
                Span::styled(format!("{state:<STATE_W$}"), state_style),
                Span::styled(format!("{input:>VALUE_W$}"), Style::default().fg(theme.fg())),
                Span::styled(format!("{output:>VALUE_W$}"), Style::default().fg(theme.fg())),
                Span::styled(format!("{total:>VALUE_W$}"), Style::default().fg(theme.fg())),
            ]));
        }
    } else {
        const STATE_W: usize = 16;
        const TOTAL_W: usize = 11;
        let round_width = body_width.saturating_sub(STATE_W + TOTAL_W).max(10);
        body.push(Line::from(Span::styled(
            format!(
                "{:<round_width$}{:<STATE_W$}{:>TOTAL_W$}",
                "Turn", "State", "Tokens"
            ),
            Style::default().fg(theme.muted()),
        )));

        for turn in round.turns.values() {
            let label = truncate_str(&turn_label(turn), round_width);
            let (state, state_style) = turn_state(turn, theme);
            let total = if turn.totals.has_tokens() {
                fmt_tokens(turn.totals.total())
            } else {
                "—".to_string()
            };
            body.push(Line::from(vec![
                Span::styled(
                    format!("{label:<round_width$}"),
                    Style::default().fg(theme.fg()),
                ),
                Span::styled(format!("{state:<STATE_W$}"), state_style),
                Span::styled(
                    format!("{total:>TOTAL_W$}"),
                    Style::default().fg(theme.fg()),
                ),
            ]));
        }
    }

    body
}

#[derive(Debug, Clone, Copy, Default)]
struct UsageTotals {
    reported_tokens: i64,
    estimated_tokens: i64,
    prompt_tokens: i64,
    completion_tokens: i64,
    cache_write_tokens: i64,
    cache_read_tokens: i64,
    known_split: bool,
    pending: bool,
}

impl UsageTotals {
    fn add_record(&mut self, record: &RequestUsageRecord) {
        self.add(
            record.source,
            record.prompt_tokens,
            record.completion_tokens,
            record.total_tokens,
            record.cache_write_tokens,
            record.cache_read_tokens,
            record.source != RequestUsageSource::Unknown,
        );
    }

    fn add_legacy(&mut self, turn: &TokenTurn) {
        self.add(
            if turn.reported {
                RequestUsageSource::Reported
            } else {
                RequestUsageSource::Estimated
            },
            turn.prompt_tokens,
            turn.completion_tokens,
            turn.total_tokens,
            turn.cache_write_tokens,
            turn.cache_read_tokens,
            turn.prompt_tokens > 0 || turn.completion_tokens > 0,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn add(
        &mut self,
        source: RequestUsageSource,
        prompt_tokens: i64,
        completion_tokens: i64,
        total_tokens: i64,
        cache_write_tokens: i64,
        cache_read_tokens: i64,
        known_split: bool,
    ) {
        match source {
            RequestUsageSource::Reported => {
                self.reported_tokens = self.reported_tokens.saturating_add(total_tokens.max(0));
            }
            RequestUsageSource::Estimated => {
                self.estimated_tokens = self.estimated_tokens.saturating_add(total_tokens.max(0));
            }
            RequestUsageSource::Unknown => {
                self.pending = true;
                return;
            }
        }
        self.prompt_tokens = self.prompt_tokens.saturating_add(prompt_tokens.max(0));
        self.completion_tokens = self
            .completion_tokens
            .saturating_add(completion_tokens.max(0));
        self.cache_write_tokens = self
            .cache_write_tokens
            .saturating_add(cache_write_tokens.max(0));
        self.cache_read_tokens = self
            .cache_read_tokens
            .saturating_add(cache_read_tokens.max(0));
        self.known_split |= known_split;
    }

    fn total(self) -> i64 {
        self.reported_tokens.saturating_add(self.estimated_tokens)
    }

    fn has_tokens(self) -> bool {
        self.total() > 0
    }
}

#[derive(Debug)]
struct RoundUsage {
    number: u64,
    totals: UsageTotals,
    turns: BTreeMap<(bool, String, u32), TurnUsage>,
}

impl RoundUsage {
    fn new(number: u64) -> Self {
        Self {
            number,
            totals: UsageTotals::default(),
            turns: BTreeMap::new(),
        }
    }

    fn add_record(&mut self, record: &RequestUsageRecord) {
        let actor = record.key.actor_id.clone();
        let key = (actor != "principal", actor.clone(), record.key.turn);
        let turn = self.turns.entry(key).or_insert_with(|| TurnUsage {
            actor,
            number: record.key.turn,
            ..Default::default()
        });
        turn.add_record(record);
        self.totals.add_record(record);
    }

    fn add_legacy(&mut self, turn: &TokenTurn) {
        let number = if turn.turn == 0 {
            self.turns
                .keys()
                .filter(|(_, actor, _)| actor == "principal")
                .map(|(_, _, number)| *number)
                .max()
                .unwrap_or(0)
                .saturating_add(1)
        } else {
            turn.turn
        };
        let item = self
            .turns
            .entry((false, "principal".to_string(), number))
            .or_insert_with(|| TurnUsage {
                actor: "principal".to_string(),
                number,
                ..Default::default()
            });
        item.add_legacy(turn);
        self.totals.add_legacy(turn);
    }
}

#[derive(Debug)]
struct TurnUsage {
    actor: String,
    number: u32,
    totals: UsageTotals,
    attempt_count: usize,
    latest_attempt: u32,
    latest_status: RequestUsageStatus,
}

impl Default for TurnUsage {
    fn default() -> Self {
        Self {
            actor: "principal".to_string(),
            number: 0,
            totals: UsageTotals::default(),
            attempt_count: 0,
            latest_attempt: 0,
            latest_status: RequestUsageStatus::InFlight,
        }
    }
}

impl TurnUsage {
    fn add_record(&mut self, record: &RequestUsageRecord) {
        self.attempt_count = self.attempt_count.saturating_add(1);
        if record.key.attempt >= self.latest_attempt {
            self.latest_attempt = record.key.attempt;
            self.latest_status = record.status;
        }
        self.totals.add_record(record);
    }

    fn add_legacy(&mut self, turn: &TokenTurn) {
        self.attempt_count = self.attempt_count.saturating_add(1);
        self.latest_attempt = self.latest_attempt.max(1);
        self.latest_status = RequestUsageStatus::Completed;
        self.totals.add_legacy(turn);
    }
}

/// Regroup the provider/model ledger rows into the user-facing Round -> Turn
/// hierarchy. Lifecycle records are authoritative when present; `turns` is
/// retained as a fallback for legacy in-memory bookings.
fn usage_rounds(report: &TokenSourceReport) -> Vec<RoundUsage> {
    let mut rounds = BTreeMap::<u64, RoundUsage>::new();
    for row in &report.rows {
        if row.requests.is_empty() {
            for turn in &row.turns {
                rounds
                    .entry(turn.round)
                    .or_insert_with(|| RoundUsage::new(turn.round))
                    .add_legacy(turn);
            }
        } else {
            for record in &row.requests {
                rounds
                    .entry(record.key.round)
                    .or_insert_with(|| RoundUsage::new(record.key.round))
                    .add_record(record);
            }
        }
    }
    rounds.into_values().collect()
}

fn section_heading(text: &str, theme: &Theme) -> Line<'static> {
    Line::from(Span::styled(
        text.to_string(),
        Style::default()
            .fg(theme.brand())
            .add_modifier(Modifier::BOLD),
    ))
}

fn round_label(number: u64) -> String {
    if number == 0 {
        "Earlier usage".to_string()
    } else {
        format!("Round {number}")
    }
}

fn turn_label(turn: &TurnUsage) -> String {
    let base = if turn.actor == "principal" {
        format!("Turn {}", turn.number)
    } else {
        format!("Envoy · T{}", turn.number)
    };
    if turn.attempt_count > 1 {
        format!("{base} ×{}", turn.attempt_count)
    } else {
        base
    }
}

fn turn_state(turn: &TurnUsage, theme: &Theme) -> (String, Style) {
    let (state, color) = match turn.latest_status {
        RequestUsageStatus::InFlight => ("in flight", theme.info()),
        RequestUsageStatus::Completed => ("completed", theme.ok()),
        RequestUsageStatus::Interrupted => ("interrupted", theme.warn()),
        RequestUsageStatus::Failed => ("failed", theme.err()),
        RequestUsageStatus::Abandoned => ("abandoned", theme.warn()),
    };
    (state.to_string(), Style::default().fg(color))
}

fn rule(width: usize, theme: &Theme) -> Line<'static> {
    Line::from(Span::styled(
        "─".repeat(width),
        Style::default().fg(theme.muted()),
    ))
}

fn kv_styled(key: &str, value: &str, value_style: Style, theme: &Theme) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{key:<22}"), Style::default().fg(theme.muted())),
        Span::styled(value.to_string(), value_style),
    ])
}

fn fmt_tokens(n: i64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

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

fn truncate_str(text: &str, max_width: usize) -> String {
    if text.width() <= max_width {
        text.to_string()
    } else if max_width <= 1 {
        "…".to_string()
    } else {
        let mut output = text
            .chars()
            .take(max_width.saturating_sub(1))
            .collect::<String>();
        output.push('…');
        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use neenee_core::ContextTokenSource;

    fn body_text(lines: &[Line<'_>]) -> String {
        lines
            .iter()
            .flat_map(|line| line.spans.iter().map(|span| span.content.as_ref()))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn context_size_is_plain_without_provenance_legend() {
        let theme = Theme::default();
        let report = TokenSourceReport::default();
        let body = list_body(
            &report,
            Some(ContextTokenSnapshot {
                tokens: 12_500,
                source: ContextTokenSource::Projection,
            }),
            200_000,
            None,
            0,
            80,
            &theme,
        );
        let text = body_text(&body);
        let size = body
            .iter()
            .flat_map(|line| &line.spans)
            .find(|span| span.content.contains("12.5k / 200.0k"))
            .expect("context size span");

        assert!(text.contains("Current AI-visible context"));
        // Provenance legend and source styling have been removed for a calmer
        // palette; values are plain foreground, not color/underline-coded.
        assert!(!text.contains("Provider-reported"));
        assert!(!text.contains("Local estimate"));
        assert!(!text.contains("Style"));
        assert!(size.style.add.is_empty());
    }

    #[test]
    fn request_usage_groups_provider_rows_by_round_then_turn() {
        let theme = Theme::default();
        let ledger = neenee_core::TokenSourceLedger::new();

        let first = ledger.begin_request("session", "relay", "model-a", 2, 1, 800);
        ledger.settle_request(&first, RequestUsageStatus::Interrupted, None, 20);
        let retry = ledger.begin_request("session", "relay", "model-a", 2, 1, 800);
        ledger.settle_request(
            &retry,
            RequestUsageStatus::Completed,
            Some(neenee_core::TokenUsage {
                prompt_tokens: 790,
                completion_tokens: 40,
                total_tokens: 830,
                ..Default::default()
            }),
            0,
        );
        let second_turn =
            ledger.begin_request("session", "another-provider", "model-b", 2, 2, 1_200);
        ledger.settle_request(&second_turn, RequestUsageStatus::Completed, None, 60);
        let next_round = ledger.begin_request("session", "relay", "model-a", 3, 1, 1_500);
        ledger.settle_request(&next_round, RequestUsageStatus::Completed, None, 75);
        let report = ledger.snapshot_for_session("session");

        assert_eq!(token_report_round_count(&report), 2);
        let list = body_text(&list_body(&report, None, 0, None, 0, 80, &theme));
        assert!(list.contains("Round 2"));
        assert!(list.contains("Round 3"));
        assert!(!list.contains("relay"));
        assert!(!list.contains("model-a"));
        assert!(!list.contains("Provider-reported"));

        let detail = detail_body(&report, 0, 80, &theme);
        let detail_text = body_text(&detail);
        assert!(detail_text.contains("Turn 1 ×2"));
        assert!(detail_text.contains("Turn 2"));
        assert!(detail_text.contains("2 / 3"));
        assert!(!detail_text.contains("another-provider"));
        assert!(!detail_text.contains("Provider-reported"));

        // Round total is rendered as plain foreground now (no provenance
        // color/underline encoding).
        let round_total = detail
            .iter()
            .flat_map(|line| &line.spans)
            .find(|span| span.content.trim() == "2.9k")
            .expect("round total span");
        assert!(round_total.style.add.is_empty());
    }
}
