//! Context-usage modal: current AI-visible context plus request usage grouped
//! by user turn. Opening a turn reveals the model rounds inside it; provider
//! and model remain ledger metadata rather than the report's navigation axis.
//!
//! Opened by clicking the context meter in the hint bar. Up and down select a
//! turn, Enter opens its rounds, and Esc backs out or closes. Token provenance
//! is encoded directly on values: provider-reported counts are bold, local
//! estimates are underlined, and mixed totals use both styles.

use std::collections::BTreeMap;

use neenee_core::{
    ContextTokenSnapshot, ContextTokenSource, RequestUsageRecord, RequestUsageSource,
    RequestUsageStatus, TokenRound, TokenSourceReport,
};
use neenee_tui::{
    Frame, Modifier, Style, {Line, Span},
};
use unicode_width::UnicodeWidthStr;

use super::common::placeholder;
use crate::render::Theme;
use crate::render::design::MODAL_INNER_H_PADDING;
use crate::render::primitives::{
    ContentModalSpec, FooterHint, SCROLL_EDGE_MARGIN, content_modal_area, content_modal_probe,
    modal_chrome_rows, modal_frame, modal_header, render_body, render_modal_footer,
};

/// Live context-meter values shown above the completed-request ledger.
#[derive(Debug, Clone, Copy)]
pub struct ContextUsageView {
    pub snapshot: Option<ContextTokenSnapshot>,
    pub window_tokens: usize,
}

/// Number of user turns represented by a report.
///
/// Kept beside the renderer so shell navigation and rendered grouping always
/// use the same definition of a turn.
pub fn token_report_turn_count(report: &TokenSourceReport) -> usize {
    usage_turns(report).len()
}

/// Draw the turn list or, when `detail` is set, the round breakdown for the
/// selected turn. `scroll` drives the visible body and is clamped by the shared
/// modal renderer. Returns the painted panel rectangle.
pub fn draw_token_report_modal(
    frame: &mut Frame,
    report: &TokenSourceReport,
    context: ContextUsageView,
    selected: usize,
    detail: bool,
    scroll: &mut usize,
    theme: &Theme,
) -> neenee_tui::Rect {
    let geometry = ContentModalSpec::TOKEN_REPORT;
    let probe = content_modal_probe(frame, geometry);
    let body_width = (probe.width as usize)
        .saturating_sub(2 * MODAL_INNER_H_PADDING as usize)
        .max(1);

    let turn_count = token_report_turn_count(report);
    let drill = detail && turn_count > 0;
    let selected = selected.min(turn_count.saturating_sub(1));

    let (title, body, footer): (&str, Vec<Line>, Vec<FooterHint>) = if drill {
        (
            "Turn Usage",
            detail_body(report, selected, body_width, theme),
            vec![
                FooterHint::always("↑↓", "scroll"),
                FooterHint::always("Esc", "turns"),
            ],
        )
    } else if turn_count == 0 {
        (
            "Context Usage",
            list_body(
                report,
                context.snapshot,
                context.window_tokens,
                selected,
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
                selected,
                body_width,
                theme,
            ),
            vec![
                FooterHint::always("↑↓", "select"),
                FooterHint::always("Enter", "rounds"),
                FooterHint::always("Esc", "close"),
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

/// Top level: one selectable row per user turn.
fn list_body(
    report: &TokenSourceReport,
    current_context: Option<ContextTokenSnapshot>,
    context_window: usize,
    selected: usize,
    body_width: usize,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let turns = usage_turns(report);
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
        let style = match snapshot.source {
            ContextTokenSource::Api => reported_style(theme),
            ContextTokenSource::Projection => estimated_style(theme),
        };
        body.push(kv_styled("Size", &size, style, theme));
    } else {
        body.push(placeholder(
            "Current context estimate unavailable.",
            true,
            theme.muted(),
        ));
    }

    body.push(Line::from(""));
    body.push(section_heading("Request usage", theme));
    body.extend(provenance_legend(body_width, theme));

    if turns.is_empty() {
        body.push(placeholder(
            "No model request attempts recorded yet.",
            true,
            theme.muted(),
        ));
        return body;
    }

    const TOKENS_W: usize = 12;
    const ROUNDS_W: usize = 9;
    let label_width = body_width.saturating_sub(TOKENS_W + ROUNDS_W + 3).max(10);

    body.push(Line::from(vec![
        Span::styled(
            format!("  {:<width$}", "Turn", width = label_width),
            Style::default().fg(theme.muted()),
        ),
        Span::styled(
            format!("{:>width$}", "Tokens", width = TOKENS_W),
            Style::default().fg(theme.muted()),
        ),
        Span::styled(
            format!(" {:>width$}", "Rounds", width = ROUNDS_W),
            Style::default().fg(theme.muted()),
        ),
    ]));
    body.push(rule(body_width, theme));

    for (index, turn) in turns.iter().enumerate() {
        let is_selected = index == selected;
        let marker = if is_selected { "> " } else { "  " };
        let label = truncate_str(&turn_label(turn.number), label_width);
        let label_style = if is_selected {
            Style::default()
                .fg(theme.brand())
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.fg())
        };
        let token_text = if turn.totals.has_tokens() {
            fmt_tokens(turn.totals.total())
        } else {
            "—".to_string()
        };
        let round_text = if turn.totals.pending {
            format!("{} …", turn.rounds.len())
        } else {
            format!("{} ›", turn.rounds.len())
        };

        body.push(Line::from(vec![
            Span::styled(
                format!("{marker}{label:<width$}", width = label_width),
                label_style,
            ),
            Span::styled(
                format!("{token_text:>width$}", width = TOKENS_W),
                provenance_style(&turn.totals, theme),
            ),
            Span::styled(
                format!(" {round_text:>width$}", width = ROUNDS_W),
                if turn.totals.pending {
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
            totals_style(
                report.grand_total.reported_tokens,
                report.grand_total.estimated_tokens,
                false,
                theme,
            ),
        ),
        Span::styled(
            format!(" {:>width$}", "", width = ROUNDS_W),
            Style::default(),
        ),
    ]));

    body.push(Line::from(""));
    body.push(Line::from(Span::styled(
        "Enter a turn to inspect its model rounds.",
        Style::default().fg(theme.muted()),
    )));
    body
}

/// Second level: aggregate all request attempts into the model rounds of one
/// selected user turn.
fn detail_body(
    report: &TokenSourceReport,
    selected: usize,
    body_width: usize,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let turns = usage_turns(report);
    let turn = &turns[selected];
    let mut body = Vec::new();

    body.push(section_heading(&turn_label(turn.number), theme));
    body.push(Line::from(""));
    body.push(kv_styled(
        "Total",
        &fmt_tokens(turn.totals.total()),
        provenance_style(&turn.totals, theme),
        theme,
    ));
    let attempt_count = turn
        .rounds
        .values()
        .map(|round| round.attempt_count)
        .sum::<usize>();
    body.push(kv_styled(
        "Rounds / attempts",
        &format!("{} / {attempt_count}", turn.rounds.len()),
        Style::default().fg(theme.fg()),
        theme,
    ));
    if turn.totals.known_split {
        body.push(kv_styled(
            "Input / output",
            &format!(
                "{} / {}",
                fmt_tokens(turn.totals.prompt_tokens),
                fmt_tokens(turn.totals.completion_tokens)
            ),
            provenance_style(&turn.totals, theme),
            theme,
        ));
    }

    if turn.totals.cache_read_tokens > 0 || turn.totals.cache_write_tokens > 0 {
        let uncached = (turn.totals.reported_tokens
            - turn.totals.cache_read_tokens
            - turn.totals.cache_write_tokens)
            .max(0);
        let denominator = (turn.totals.cache_read_tokens + uncached).max(1) as f64;
        let hit_rate = (turn.totals.cache_read_tokens as f64 / denominator * 100.0).round() as i64;
        body.push(kv_styled(
            "Cache read / write",
            &format!(
                "{} / {}",
                fmt_tokens(turn.totals.cache_read_tokens),
                fmt_tokens(turn.totals.cache_write_tokens)
            ),
            reported_style(theme),
            theme,
        ));
        body.push(kv_styled(
            "Cache hit rate",
            &format!("{hit_rate}%"),
            reported_style(theme),
            theme,
        ));
    }

    body.push(Line::from(""));
    body.push(section_heading("Rounds", theme));
    body.extend(provenance_legend(body_width, theme));
    body.push(rule(body_width, theme));

    let full_table = body_width >= 62;
    if full_table {
        const STATE_W: usize = 16;
        const VALUE_W: usize = 10;
        let round_width = body_width.saturating_sub(STATE_W + VALUE_W * 3).max(10);
        body.push(Line::from(Span::styled(
            format!(
                "{:<round_width$}{:<STATE_W$}{:>VALUE_W$}{:>VALUE_W$}{:>VALUE_W$}",
                "Round", "State", "Input", "Output", "Total"
            ),
            Style::default().fg(theme.muted()),
        )));

        for round in turn.rounds.values() {
            let label = truncate_str(&round_label(round), round_width);
            let (state, state_style) = round_state(round, theme);
            let (input, output) = if round.totals.known_split {
                (
                    fmt_tokens(round.totals.prompt_tokens),
                    fmt_tokens(round.totals.completion_tokens),
                )
            } else {
                ("—".to_string(), "—".to_string())
            };
            let total = if round.totals.has_tokens() {
                fmt_tokens(round.totals.total())
            } else {
                "—".to_string()
            };
            let value_style = provenance_style(&round.totals, theme);
            body.push(Line::from(vec![
                Span::styled(
                    format!("{label:<round_width$}"),
                    Style::default().fg(theme.fg()),
                ),
                Span::styled(format!("{state:<STATE_W$}"), state_style),
                Span::styled(format!("{input:>VALUE_W$}"), value_style),
                Span::styled(format!("{output:>VALUE_W$}"), value_style),
                Span::styled(format!("{total:>VALUE_W$}"), value_style),
            ]));
        }
    } else {
        const STATE_W: usize = 16;
        const TOTAL_W: usize = 11;
        let round_width = body_width.saturating_sub(STATE_W + TOTAL_W).max(10);
        body.push(Line::from(Span::styled(
            format!(
                "{:<round_width$}{:<STATE_W$}{:>TOTAL_W$}",
                "Round", "State", "Tokens"
            ),
            Style::default().fg(theme.muted()),
        )));

        for round in turn.rounds.values() {
            let label = truncate_str(&round_label(round), round_width);
            let (state, state_style) = round_state(round, theme);
            let total = if round.totals.has_tokens() {
                fmt_tokens(round.totals.total())
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
                    provenance_style(&round.totals, theme),
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

    fn add_legacy(&mut self, round: &TokenRound) {
        self.add(
            if round.reported {
                RequestUsageSource::Reported
            } else {
                RequestUsageSource::Estimated
            },
            round.prompt_tokens,
            round.completion_tokens,
            round.total_tokens,
            round.cache_write_tokens,
            round.cache_read_tokens,
            round.prompt_tokens > 0 || round.completion_tokens > 0,
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
struct TurnUsage {
    number: u64,
    totals: UsageTotals,
    rounds: BTreeMap<(bool, String, u32), RoundUsage>,
}

impl TurnUsage {
    fn new(number: u64) -> Self {
        Self {
            number,
            totals: UsageTotals::default(),
            rounds: BTreeMap::new(),
        }
    }

    fn add_record(&mut self, record: &RequestUsageRecord) {
        let actor = record.key.actor_id.clone();
        let key = (actor != "principal", actor.clone(), record.key.turn);
        let round = self.rounds.entry(key).or_insert_with(|| RoundUsage {
            actor,
            number: record.key.turn,
            ..Default::default()
        });
        round.add_record(record);
        self.totals.add_record(record);
    }

    fn add_legacy(&mut self, round: &TokenRound) {
        let number = if round.round == 0 {
            self.rounds
                .keys()
                .filter(|(_, actor, _)| actor == "principal")
                .map(|(_, _, number)| *number)
                .max()
                .unwrap_or(0)
                .saturating_add(1)
        } else {
            round.round
        };
        let item = self
            .rounds
            .entry((false, "principal".to_string(), number))
            .or_insert_with(|| RoundUsage {
                actor: "principal".to_string(),
                number,
                ..Default::default()
            });
        item.add_legacy(round);
        self.totals.add_legacy(round);
    }
}

#[derive(Debug)]
struct RoundUsage {
    actor: String,
    number: u32,
    totals: UsageTotals,
    attempt_count: usize,
    latest_attempt: u32,
    latest_status: RequestUsageStatus,
}

impl Default for RoundUsage {
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

impl RoundUsage {
    fn add_record(&mut self, record: &RequestUsageRecord) {
        self.attempt_count = self.attempt_count.saturating_add(1);
        if record.key.attempt >= self.latest_attempt {
            self.latest_attempt = record.key.attempt;
            self.latest_status = record.status;
        }
        self.totals.add_record(record);
    }

    fn add_legacy(&mut self, round: &TokenRound) {
        self.attempt_count = self.attempt_count.saturating_add(1);
        self.latest_attempt = self.latest_attempt.max(1);
        self.latest_status = RequestUsageStatus::Completed;
        self.totals.add_legacy(round);
    }
}

/// Regroup the provider/model ledger rows into the user-facing Turn -> Round
/// hierarchy. Lifecycle records are authoritative when present; `rounds` is
/// retained as a fallback for legacy in-memory bookings.
fn usage_turns(report: &TokenSourceReport) -> Vec<TurnUsage> {
    let mut turns = BTreeMap::<u64, TurnUsage>::new();
    for row in &report.rows {
        if row.requests.is_empty() {
            for round in &row.rounds {
                turns
                    .entry(round.turn)
                    .or_insert_with(|| TurnUsage::new(round.turn))
                    .add_legacy(round);
            }
        } else {
            for record in &row.requests {
                turns
                    .entry(record.key.round)
                    .or_insert_with(|| TurnUsage::new(record.key.round))
                    .add_record(record);
            }
        }
    }
    turns.into_values().collect()
}

fn provenance_legend(body_width: usize, theme: &Theme) -> Vec<Line<'static>> {
    if body_width >= 54 {
        vec![Line::from(vec![
            Span::styled("Style  ", Style::default().fg(theme.muted())),
            Span::styled("Provider-reported", reported_style(theme)),
            Span::styled("  ", Style::default()),
            Span::styled("Local estimate", estimated_style(theme)),
            Span::styled("  ", Style::default()),
            Span::styled("Mixed", mixed_style(theme)),
        ])]
    } else {
        vec![
            Line::from(vec![
                Span::styled("Style  ", Style::default().fg(theme.muted())),
                Span::styled("Provider-reported", reported_style(theme)),
            ]),
            Line::from(Span::styled("Local estimate", estimated_style(theme))),
            Line::from(Span::styled("Mixed", mixed_style(theme))),
        ]
    }
}

fn section_heading(text: &str, theme: &Theme) -> Line<'static> {
    Line::from(Span::styled(
        text.to_string(),
        Style::default()
            .fg(theme.brand())
            .add_modifier(Modifier::BOLD),
    ))
}

fn turn_label(number: u64) -> String {
    if number == 0 {
        "Earlier usage".to_string()
    } else {
        format!("Turn {number}")
    }
}

fn round_label(round: &RoundUsage) -> String {
    let base = if round.actor == "principal" {
        format!("Round {}", round.number)
    } else {
        format!("Envoy · R{}", round.number)
    };
    if round.attempt_count > 1 {
        format!("{base} ×{}", round.attempt_count)
    } else {
        base
    }
}

fn round_state(round: &RoundUsage, theme: &Theme) -> (String, Style) {
    let (state, color) = match round.latest_status {
        RequestUsageStatus::InFlight => ("in flight", theme.info()),
        RequestUsageStatus::Completed => ("completed", theme.ok()),
        RequestUsageStatus::Interrupted => ("interrupted", theme.warn()),
        RequestUsageStatus::Failed => ("failed", theme.err()),
        RequestUsageStatus::Abandoned => ("abandoned", theme.warn()),
    };
    (state.to_string(), Style::default().fg(color))
}

fn provenance_style(totals: &UsageTotals, theme: &Theme) -> Style {
    totals_style(
        totals.reported_tokens,
        totals.estimated_tokens,
        totals.pending,
        theme,
    )
}

fn totals_style(reported: i64, estimated: i64, pending: bool, theme: &Theme) -> Style {
    if reported > 0 && estimated > 0 {
        mixed_style(theme)
    } else if reported > 0 {
        reported_style(theme)
    } else if estimated > 0 {
        estimated_style(theme)
    } else if pending {
        Style::default().fg(theme.info())
    } else {
        Style::default().fg(theme.muted())
    }
}

fn reported_style(theme: &Theme) -> Style {
    Style::default().fg(theme.ok()).add_modifier(Modifier::BOLD)
}

fn estimated_style(theme: &Theme) -> Style {
    Style::default()
        .fg(theme.warn())
        .add_modifier(Modifier::UNDERLINED)
}

fn mixed_style(theme: &Theme) -> Style {
    Style::default()
        .fg(theme.warn())
        .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
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

    fn body_text(lines: &[Line<'_>]) -> String {
        lines
            .iter()
            .flat_map(|line| line.spans.iter().map(|span| span.content.as_ref()))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn context_projection_uses_estimate_style_and_a_compact_legend() {
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
        let size = body
            .iter()
            .flat_map(|line| &line.spans)
            .find(|span| span.content.contains("12.5k / 200.0k"))
            .expect("context size span");

        assert!(text.contains("Current AI-visible context"));
        assert!(text.contains("Provider-reported"));
        assert!(text.contains("Local estimate"));
        assert!(!text.contains("local request projection (estimated)"));
        assert!(size.style.add.contains(Modifier::UNDERLINED));
        assert!(!size.style.add.contains(Modifier::BOLD));
    }

    #[test]
    fn request_usage_groups_provider_rows_by_turn_then_round() {
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
        let second_round =
            ledger.begin_request("session", "another-provider", "model-b", 2, 2, 1_200);
        ledger.settle_request(&second_round, RequestUsageStatus::Completed, None, 60);
        let next_turn = ledger.begin_request("session", "relay", "model-a", 3, 1, 1_500);
        ledger.settle_request(&next_turn, RequestUsageStatus::Completed, None, 75);
        let report = ledger.snapshot_for_session("session");

        assert_eq!(token_report_turn_count(&report), 2);
        let list = body_text(&list_body(&report, None, 0, 0, 80, &theme));
        assert!(list.contains("Turn 2"));
        assert!(list.contains("Turn 3"));
        assert!(!list.contains("relay"));
        assert!(!list.contains("model-a"));

        let detail = detail_body(&report, 0, 80, &theme);
        let detail_text = body_text(&detail);
        assert!(detail_text.contains("Round 1 ×2"));
        assert!(detail_text.contains("Round 2"));
        assert!(detail_text.contains("2 / 3"));
        assert!(!detail_text.contains("another-provider"));

        let turn_total = detail
            .iter()
            .flat_map(|line| &line.spans)
            .find(|span| span.content.trim() == "2.9k")
            .expect("mixed turn total");
        assert!(turn_total.style.add.contains(Modifier::BOLD));
        assert!(turn_total.style.add.contains(Modifier::UNDERLINED));
    }
}
