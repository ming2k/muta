//! Context-usage modal: current AI-visible context plus request usage grouped
//! by user round. Opening a round reveals the model turns inside it; provider
//! and model remain ledger metadata rather than the report's navigation axis.
//!
//! Opened by clicking the context meter in the hint bar. Up and down select a
//! round, Enter opens its turns, and Esc backs out or closes. Values use a
//! calm, single-foreground palette; only turn lifecycle state is colored.

use std::collections::BTreeMap;

use muta_contracts::{
    ContextTokenSnapshot, RequestUsageRecord, RequestUsageSource, RequestUsageStatus,
    TokenSourceReport, TokenTurn,
};
use mutx_engine::{
    Frame, Modifier, Style, {Line, Span},
};
use unicode_width::UnicodeWidthStr;

use super::common::placeholder;
use crate::components::selectable_body::{SelectableRow, render_selectable_body};
use crate::design::MODAL_INNER_H_PADDING;
use crate::primitives::{
    ContentModalSpec, FooterHint, HeaderPart, SCROLL_EDGE_MARGIN, breadcrumb_parts,
    content_modal_area, content_modal_probe, keyvocab, modal_chrome_rows, modal_frame,
    modal_header_parts, render_body, render_modal_footer,
};
use crate::view::Theme;

/// Live context-meter values shown above the completed-request ledger.
#[derive(Debug, Clone, Copy)]
pub struct ContextUsageView {
    pub snapshot: Option<ContextTokenSnapshot>,
    pub window_tokens: usize,
    pub draft_tokens: usize,
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
///
/// `loading` marks the attach-mode round-trip: the frontend dispatched
/// `QueryTokenUsage` and is waiting for the daemon's report. The body then
/// shows a loading placeholder instead of the empty-ledger copy, so a
/// not-yet-arrived report never reads as "no usage recorded".
#[allow(clippy::too_many_arguments)] // modal draw fns thread many context args by nature
pub fn draw_token_report_modal(
    frame: &mut Frame,
    report: &TokenSourceReport,
    context: ContextUsageView,
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

    let round_count = token_report_round_count(report);
    let drill = detail && round_count > 0;
    let selected = selected.min(round_count.saturating_sub(1));

    // Compute the drill-in breadcrumb child once at function scope so the
    // borrowed `HeaderPart`s live long enough to reach the header render.
    // Only meaningful when `drill`; empty otherwise.
    let drill_child = if drill {
        let round = &usage_rounds(report)[selected];
        round_label(round.number)
    } else {
        String::new()
    };

    let (header, body, footer, follow): (
        Vec<HeaderPart<'_>>,
        Vec<Line>,
        Vec<FooterHint>,
        Option<usize>,
    ) = if loading && !drill {
        (
            vec![HeaderPart::title("Context Usage")],
            vec![placeholder(
                "Loading token usage from the daemon…",
                true,
                theme.muted(),
            )],
            vec![FooterHint::always(keyvocab::ESC, "close")],
            None,
        )
    } else if drill {
        // The drill-in sub-page keeps the same modal but switches its header
        // to a breadcrumb ("Context Usage › 1st round") so the user sees
        // where they are in the hierarchy.
        let header = breadcrumb_parts("Context Usage", &drill_child).to_vec();
        (
            header,
            detail_body(report, selected, body_width, theme),
            vec![
                FooterHint::always(keyvocab::ARROWS_UD, "scroll"),
                FooterHint::always(keyvocab::ESC, "rounds"),
            ],
            None,
        )
    } else if round_count == 0 {
        let (body, follow) = list_body(
            report,
            context.snapshot,
            context.window_tokens,
            context.draft_tokens,
            selected,
            body_width,
            theme,
        );
        (
            vec![HeaderPart::title("Context Usage")],
            body,
            vec![FooterHint::always(keyvocab::ESC, "close")],
            follow,
        )
    } else {
        let (body, follow) = list_body(
            report,
            context.snapshot,
            context.window_tokens,
            context.draft_tokens,
            selected,
            body_width,
            theme,
        );
        (
            vec![HeaderPart::title("Context Usage")],
            body,
            vec![
                FooterHint::always(keyvocab::ARROWS_UD, "select"),
                FooterHint::always(keyvocab::ENTER, "turns"),
                FooterHint::always(keyvocab::ESC, "close"),
            ],
            follow,
        )
    };

    // The drill-in sub-view is a selectable document (KV read-out, turn
    // table, provenance legend — the numbers a user copies out of this
    // modal). The round list stays a picker: its rows are ↑/↓ + Enter
    // targets, and selection there would fight the click affordances.
    let desired = body.len() as u16 + modal_chrome_rows(geometry.modal_spec());
    let area = content_modal_area(frame, geometry, desired);
    let modal = modal_frame(frame, area, theme.panel(), true, true);

    modal_header_parts(frame, modal.header, &header, theme);
    if drill {
        let rows: Vec<SelectableRow> = body.into_iter().map(SelectableRow::from_line).collect();
        render_selectable_body(
            frame, modal.body, &rows, scroll, follow, theme, selection, layout_map,
        );
    } else {
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
    }

    if let Some(footer_area) = modal.footer {
        render_modal_footer(frame, footer_area, &footer, theme);
    }
    area
}

/// Top level: one selectable row per user round. Returns the body lines and
/// the index of the selected row (for auto-scroll `follow`), if any.
fn list_body(
    report: &TokenSourceReport,
    current_context: Option<ContextTokenSnapshot>,
    context_window: usize,
    draft_tokens: usize,
    selected: usize,
    body_width: usize,
    theme: &Theme,
) -> (Vec<Line<'static>>, Option<usize>) {
    let rounds = usage_rounds(report);
    let mut body = Vec::new();
    let mut selected_line: Option<usize> = None;

    if let Some(snapshot) = current_context {
        let total_with_draft = snapshot.tokens.saturating_add(draft_tokens);
        let size = if context_window > 0 {
            let ratio = (total_with_draft as f64 / context_window as f64).clamp(0.0, 1.0);
            if draft_tokens > 0 {
                format!(
                    "{} (+{}) / {}  ({}%)",
                    fmt_token_count(snapshot.tokens),
                    fmt_token_count(draft_tokens),
                    fmt_token_count(context_window),
                    (ratio * 100.0).round() as u32,
                )
            } else {
                format!(
                    "{} / {}  ({}%)",
                    fmt_token_count(snapshot.tokens),
                    fmt_token_count(context_window),
                    (ratio * 100.0).round() as u32,
                )
            }
        } else if draft_tokens > 0 {
            format!(
                "{} (+{})",
                fmt_token_count(snapshot.tokens),
                fmt_token_count(draft_tokens)
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
        if let Some(overhead) = snapshot.overhead_tokens {
            body.push(kv_styled(
                "Base overhead",
                &format!("{} (system prompt & tools)", fmt_token_count(overhead)),
                Style::default().fg(theme.muted()),
                theme,
            ));
        }
        if let Some(history) = snapshot.history_tokens {
            body.push(kv_styled(
                "Conversation",
                &format!("{} (history)", fmt_token_count(history)),
                Style::default().fg(theme.muted()),
                theme,
            ));
        }
        if draft_tokens > 0 {
            body.push(kv_styled(
                "Draft prompt",
                &format!("{} (composer input)", fmt_token_count(draft_tokens)),
                Style::default().fg(theme.info()),
                theme,
            ));
        }
    } else {
        body.push(placeholder(
            "Current context estimate unavailable.",
            true,
            theme.muted(),
        ));
    }

    // Session-average model output rate: every settled attempt's output
    // tokens divided by the total time the model actually spent generating
    // (excluding tool execution, hooks, and human-decision pauses, but
    // including any envoy's generation once its output tokens are counted
    // too), so this reflects the server's real efficiency rather than how
    // long tools ran or how long the user deliberated. "Output rate" is more
    // honest than "Throughput" — throughput implies end-to-end processing
    // speed, whereas this deliberately excludes everything except model
    // generation.
    //
    // This is the *session-wide* mean, not the latest round's: summing
    // token-time across attempts (Σ tokens / Σ ms, weighted by how long each
    // request actually streamed) rather than averaging per-round rates, so a
    // short fast round cannot drag a long slow one up. Only attempts that
    // have a per-attempt rate at all contribute (measured span and booked
    // output tokens), and it divides exactly the quantities the per-round
    // TPS column and the per-attempt `tok/s` column divide, so all three
    // views always agree. The per-round TPS column in the table below
    // carries the per-round story.
    let session_rate = session_output_rate(report);
    let session_rate_label = fmt_rate_label(session_rate);
    body.push(kv_styled(
        "Output rate",
        &session_rate_label,
        Style::default().fg(theme.fg()),
        theme,
    ));

    body.push(Line::from(""));

    if rounds.is_empty() {
        body.push(placeholder(
            "No model request attempts recorded yet.",
            true,
            theme.muted(),
        ));
        return (body, None);
    }

    // The round table has four content-sized columns — Round, State, Tokens,
    // TPS — and any leftover modal width is split evenly across the gaps
    // between them, so the columns breathe instead of clumping at the left.
    // The TPS column is this round's average output rate (output tokens ÷
    // the round's total measured generation time); the turn count it
    // replaced lives on in the drill-in's "Turns / attempts" row.
    let mut tokens_w = "Tokens".width();
    let mut rate_w = TPS_HEADER.len();
    let mut state_w = "State".width();
    for round in &rounds {
        let t = if round.totals.has_tokens() {
            fmt_tokens(round.totals.total())
        } else {
            "—".to_string()
        };
        tokens_w = tokens_w.max(t.width());
        rate_w = rate_w.max(fmt_round_rate(round).width());
        state_w = state_w.max(round_state(round).width());
    }
    // The label column is sized to its content (header + bare ordinals),
    // capped so a very long label truncates rather than crowding the others.
    let label_budget = body_width
        .saturating_sub(state_w + tokens_w + rate_w)
        .max(8);
    let label_w = ["Round"]
        .into_iter()
        .map(str::width)
        .chain(rounds.iter().map(|r| round_row_label(r.number).width()))
        .map(|w| w.min(label_budget))
        .max()
        .unwrap_or(0);

    // Remaining width after all columns → split into equal gaps between the
    // four columns (3 inter-column gaps) plus a small left indent.
    const LEFT_INSET: usize = 2;
    let used = LEFT_INSET + label_w + state_w + tokens_w + rate_w;
    let gaps = 3usize;
    let total_gap = body_width.saturating_sub(used);
    // Distribute remainder as evenly as possible; leftover cells go to the
    // earlier gaps (keeps the row exactly `body_width` wide).
    let base_gap = total_gap / gaps;
    let extra = total_gap % gaps;
    let gap_w = |i: usize| base_gap + if i < extra { 1 } else { 0 };

    let header_bg = theme.panel();
    let pad_span = |text: &str, width: usize, style: Style| {
        Span::styled(format_padded_left(text, width), style)
    };
    let gap_span = |i: usize, bg| Span::styled(" ".repeat(gap_w(i)), Style::default().bg(bg));

    body.push(Line::from(vec![
        Span::styled(" ".repeat(LEFT_INSET), Style::default().bg(header_bg)),
        pad_span(
            "Round",
            label_w,
            Style::default().bg(header_bg).fg(theme.muted()),
        ),
        gap_span(0, header_bg),
        pad_span(
            "State",
            state_w,
            Style::default().bg(header_bg).fg(theme.muted()),
        ),
        gap_span(1, header_bg),
        Span::styled(
            format!("{:>width$}", "Tokens", width = tokens_w),
            Style::default().bg(header_bg).fg(theme.muted()),
        ),
        gap_span(2, header_bg),
        Span::styled(
            format!("{:>width$}", TPS_HEADER, width = rate_w),
            Style::default().bg(header_bg).fg(theme.muted()),
        ),
    ]));

    for (index, round) in rounds.iter().enumerate() {
        let is_selected = index == selected;
        // Selection is shown purely by a subtle full-width background
        // highlight (no arrow marker, no bold text) — the established
        // convention for selectable rows elsewhere in this modal family.
        let bg = if is_selected {
            theme.selected()
        } else {
            theme.panel()
        };
        let label = truncate_str(&round_row_label(round.number), label_budget);
        let token_text = if round.totals.has_tokens() {
            fmt_tokens(round.totals.total())
        } else {
            "—".to_string()
        };
        let rate_text = fmt_round_rate(round);
        let (state_text, state_style) = round_state_styled(round, theme);

        if is_selected {
            selected_line = Some(body.len());
        }
        body.push(Line::from(vec![
            Span::styled(" ".repeat(LEFT_INSET), Style::default().bg(bg)),
            pad_span(&label, label_w, Style::default().bg(bg).fg(theme.fg())),
            gap_span(0, bg),
            pad_span(state_text, state_w, state_style.bg(bg)),
            gap_span(1, bg),
            Span::styled(
                format!("{token_text:>width$}", width = tokens_w),
                Style::default().bg(bg).fg(theme.fg()),
            ),
            gap_span(2, bg),
            Span::styled(
                format!("{rate_text:>width$}", width = rate_w),
                Style::default().bg(bg).fg(theme.muted()),
            ),
        ]));
    }

    (body, selected_line)
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
        // "hit" vs "populate": a read is a *cache hit* (discounted reuse);
        // a write is the *first-time population* of the provider cache this turn
        // (billed at a premium so later turns can hit). The hit rate rides along
        // in parentheses so it reads as one fact, not a separate metric.
        body.push(kv_styled(
            "Cache hit",
            &format!(
                "{} ({hit_rate}%)",
                fmt_tokens(round.totals.cache_read_tokens)
            ),
            Style::default().fg(theme.fg()),
            theme,
        ));
        body.push(kv_styled(
            "Cache populate",
            &fmt_tokens(round.totals.cache_write_tokens),
            Style::default().fg(theme.fg()),
            theme,
        ));
    }

    body.push(Line::from(""));
    body.push(section_heading("Turns", theme));

    // The table is one flat list of the principal's own attempts, newest-first.
    // (Envoy sub-conversations are forks — their usage belongs to the fork's own
    // context, so they are not shown here.) One row per attempt: a single-
    // attempt turn shows a bare ordinal ("1st"); a retried turn shows its later
    // attempts as "<turn> - <attempt>" ("1st - 2nd").
    let attempts = flat_attempts(report, round.number);

    // Token totals are tinted by their provenance: green when fully reported
    // by the provider, yellow when estimated locally, plain when unknown. A
    // legend is shown beneath the table.
    //
    // Width pressure degrades the table in discrete steps rather than by
    // squeezing gutters: the Turn label column flexes first (down to a small
    // floor), then whole *column groups* drop as atoms — never a gap shrink.
    // Columns are dropped from the left so the right edge keeps hugging the
    // modal margin at every tier:
    //   ≥62   input · output · total · tok/s
    //   ≥42   tokens · tok/s
    //   ≥32   tok/s only
    //   <32   state alone
    let columns = AttemptColumns::for_width(body_width);
    columns.push_header(&mut body, body_width, theme);
    for a in &attempts {
        columns.push_row(&mut body, a, body_width, theme);
    }

    body.push(Line::from(""));
    // The provenance legend is responsive: the full single-line form when it
    // fits, otherwise the shortened "green/yellow = reported/estimated" form
    // (wrapping onto a second line as a last resort), so a narrow modal never
    // clips the explanation mid-word.
    for spans in legend_lines(body_width, theme) {
        body.push(Line::from(spans));
    }

    body
}

/// Legend beneath the turns table, as one or two span rows. The widest form
/// ("Tokens:  green = provider-reported   yellow = local estimate") is
/// preferred; when it exceeds `body_width` the wording collapses to
/// "Tokens:  green reported   yellow estimated" — or, narrower still, the
/// even shorter "Tokens:  green reported" / "yellow estimated" pair, and
/// finally a two-line layout so nothing is ever truncated.
fn legend_lines(body_width: usize, theme: &Theme) -> Vec<Vec<Span<'static>>> {
    let muted = Style::default().fg(theme.muted());
    let green = Style::default().fg(theme.ok());
    let yellow = Style::default().fg(theme.warn());

    let full: Vec<Span<'static>> = vec![
        Span::styled("Tokens:  ".to_string(), muted),
        Span::styled("green", green),
        Span::styled(" = provider-reported   ".to_string(), muted),
        Span::styled("yellow", yellow),
        Span::styled(" = local estimate".to_string(), muted),
    ];
    let short: Vec<Span<'static>> = vec![
        Span::styled("Tokens:  ".to_string(), muted),
        Span::styled("green", green),
        Span::styled(" reported   ".to_string(), muted),
        Span::styled("yellow", yellow),
        Span::styled(" estimated".to_string(), muted),
    ];
    let short_green: Vec<Span<'static>> = vec![
        Span::styled("Tokens:  ".to_string(), muted),
        Span::styled("green", green),
        Span::styled(" reported".to_string(), muted),
    ];
    let short_yellow: Vec<Span<'static>> = vec![
        Span::styled("yellow", yellow),
        Span::styled(" estimated".to_string(), muted),
    ];

    let width_of = |spans: &[Span<'static>]| spans_width(spans);
    if width_of(&full) <= body_width {
        vec![full]
    } else if width_of(&short) <= body_width {
        vec![short]
    } else if width_of(&short_green) <= body_width && width_of(&short_yellow) <= body_width {
        vec![short_green, short_yellow]
    } else {
        vec![
            short_green,
            vec![Span::styled("yellow".to_string(), yellow)],
            vec![Span::styled("estimated".to_string(), muted)],
        ]
    }
}

/// Total display width of a span row, used to pick a legend variant that
/// never overflows the modal body.
fn spans_width(spans: &[Span<'static>]) -> usize {
    spans.iter().map(|span| span.content.as_ref().width()).sum()
}

/// Width budget of one right-aligned atomic column unit: the value plus the
/// single gutter that separates it from whatever stands to its left. Units
/// are added or dropped *whole* as the modal narrows — the gutter is never
/// squeezed cell by cell.
const ATTEMPT_COLUMN_W: usize = 10;
/// Fixed width of the left-aligned State column: the longest state label
/// ("interrupted") plus breathing room.
const ATTEMPT_STATE_W: usize = 16;
/// The Turn label column is the flexible one, absorbing slack above each
/// tier's threshold; it never shrinks below this floor before the narrowest
/// column group drops as a unit instead.
const ATTEMPT_LABEL_MIN: usize = 6;

/// The four-column layout ladder for the detail turns table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AttemptColumns {
    /// Turn · State · Input · Output · Total · tok/s (≥62 cols).
    Split,
    /// Turn · State · Tokens · tok/s (≥46 cols).
    Total,
    /// Turn · State · tok/s (≥37 cols).
    Rate,
    /// Turn · State (<37 cols).
    State,
}

impl AttemptColumns {
    /// Pick the widest layout whose columns fit `body_width`: each tier's
    /// threshold is `label floor + state + columns × atomic unit`.
    fn for_width(body_width: usize) -> Self {
        let fits = |columns: usize| {
            body_width >= ATTEMPT_LABEL_MIN + ATTEMPT_STATE_W + columns * ATTEMPT_COLUMN_W
        };
        if fits(4) {
            Self::Split
        } else if fits(2) {
            Self::Total
        } else if fits(1) {
            Self::Rate
        } else {
            Self::State
        }
    }

    /// Number of atomic right-aligned columns this layout carries.
    fn column_count(&self) -> usize {
        match self {
            Self::Split => 4,
            Self::Total => 2,
            Self::Rate => 1,
            Self::State => 0,
        }
    }

    /// Width left for the flexible Turn label column.
    fn label_width(&self, body_width: usize) -> usize {
        body_width
            .saturating_sub(ATTEMPT_STATE_W + self.column_count() * ATTEMPT_COLUMN_W)
            .max(ATTEMPT_LABEL_MIN)
    }

    fn push_header(&self, body: &mut Vec<Line<'static>>, body_width: usize, theme: &Theme) {
        let muted = Style::default().fg(theme.muted());
        let label_w = self.label_width(body_width);
        let mut spans = vec![
            Span::styled(format_padded_left("Turn", label_w), muted),
            Span::styled(format_padded_left("State", ATTEMPT_STATE_W), muted),
        ];
        let headers: &[&str] = match self {
            Self::Split => &["Input", "Output", "Total", "tok/s"],
            Self::Total => &["Tokens", "tok/s"],
            Self::Rate => &["tok/s"],
            Self::State => &[],
        };
        for title in headers {
            spans.push(Span::styled(format!("{title:>ATTEMPT_COLUMN_W$}"), muted));
        }
        body.push(Line::from(spans));
    }

    /// One attempt row: flexible label · state · the layout's atomic columns.
    /// Token counts are tinted by the attempt's source; the tok/s cell stays
    /// muted (a pace metric, not a billing count).
    fn push_row(
        &self,
        body: &mut Vec<Line<'static>>,
        a: &FlatAttempt<'_>,
        body_width: usize,
        theme: &Theme,
    ) {
        let rec = a.record;
        let label_w = self.label_width(body_width);
        let label = truncate_str(&attempt_label(a.turn, a.attempt), label_w);
        let (state, state_style) = attempt_state(rec, theme);
        let value_style = attempt_source_style(rec, theme);
        let rate_style = Style::default().fg(theme.muted());

        let known_split = rec.prompt_tokens > 0 || rec.completion_tokens > 0;
        let (input, output) = if known_split {
            (
                fmt_tokens(rec.prompt_tokens),
                fmt_tokens(rec.completion_tokens),
            )
        } else {
            ("—".to_string(), "—".to_string())
        };
        let total = if rec.total_tokens > 0 {
            fmt_tokens(rec.total_tokens)
        } else {
            "—".to_string()
        };
        let rate = fmt_attempt_rate(rec);

        // Cells are appended left-to-right in the same atomic units as the
        // header, so the tok/s column always hugs the right modal margin.
        let cells: Vec<(String, Style)> = match self {
            Self::Split => vec![
                (input, value_style),
                (output, value_style),
                (total, value_style),
                (rate, rate_style),
            ],
            Self::Total => vec![(total, value_style), (rate, rate_style)],
            Self::Rate => vec![(rate, rate_style)],
            Self::State => vec![],
        };

        let mut spans = vec![
            Span::styled(
                format_padded_left(&label, label_w),
                Style::default().fg(theme.fg()),
            ),
            Span::styled(format_padded_left(state, ATTEMPT_STATE_W), state_style),
        ];
        for (text, style) in cells {
            spans.push(Span::styled(format!("{text:>ATTEMPT_COLUMN_W$}"), style));
        }
        body.push(Line::from(spans));
        if let Some(err) = rec.error.as_deref().filter(|e| !e.is_empty()) {
            body.push(Line::from(vec![
                Span::styled("  ↳ ".to_string(), Style::default().fg(theme.err())),
                Span::styled(
                    truncate_str(err, body_width.saturating_sub(4)),
                    Style::default().fg(theme.muted()),
                ),
            ]));
        }
    }
}

/// One attempt's settled output rate: completion tokens ÷ measured generation
/// span. Renders `–` whenever the rate would be meaningless: the attempt is
/// still in flight, no generation span was measured (failed before a validated
/// response, or persisted before timing was recorded), or the completion side
/// was never booked. Untinted by provenance — it is a pace metric, not a
/// billing count, so it stays muted next to the tinted token columns.
fn fmt_attempt_rate(record: &RequestUsageRecord) -> String {
    if record.status == RequestUsageStatus::InFlight || record.generation_ms == 0 {
        return "–".to_string();
    }
    if record.completion_tokens <= 0 {
        return "–".to_string();
    }
    let rate = record.completion_tokens as f64 * 1000.0 / record.generation_ms as f64;
    if rate > 0.0 && rate < 10.0 {
        // Below ~10 tok/s one decimal keeps 8.5 from reading as 9 while the
        // cell stays within the fixed 9-column width.
        format!("{rate:.1}")
    } else {
        format!("{rate:.0}")
    }
}

/// Header label of the round table's per-round average output-rate column.
/// "TPS" (tokens per second) matches the modal's TUI-wide throughput
/// vocabulary; the drill-in's per-attempt column keeps its finer "tok/s"
/// spelling.
const TPS_HEADER: &str = "TPS";

/// Format a tok/s value for a cell: integer when ≥10, one decimal below
/// that (so 8.5 does not read as 9), `–` when there is no rate.
fn fmt_rate_value(rate: f64) -> String {
    if rate > 0.0 && rate < 10.0 {
        format!("{rate:.1}")
    } else if rate >= 10.0 {
        format!("{rate:.0}")
    } else {
        "–".to_string()
    }
}

/// Same rate with the ` tok/s` unit, for the key/value summary row. Always
/// one decimal — unlike the width-constrained table cells, this line has
/// room, and a session average over many requests is rarely a round number.
fn fmt_rate_label(rate: f64) -> String {
    if rate > 0.0 {
        format!("{rate:.1} tok/s")
    } else {
        "–".to_string()
    }
}

/// The round's average output rate: the output tokens the round's *timed*
/// attempts generated ÷ the generation time they actually measured (Σ tokens
/// / Σ ms, weighted by streaming time). Only attempts that have a per-attempt
/// rate at all contribute (see [`RoundUsage::timed_generation_ms`]), so this
/// is exactly the weighted mean of the drill-in's `tok/s` column — the list
/// and the detail page always agree. A round where nothing was timed renders
/// `–` rather than a fabricated figure: legacy `record*` bookings carry no
/// timing, and an in-flight attempt has not sealed its clock yet.
fn fmt_round_rate(round: &RoundUsage) -> String {
    if round.timed_generation_ms == 0 || round.timed_output_tokens <= 0 {
        return "–".to_string();
    }
    fmt_rate_value(round.timed_output_tokens as f64 * 1000.0 / round.timed_generation_ms as f64)
}

/// Whole-session average output rate across every terminal attempt in the
/// report, including envoy sub-conversations (symmetrically: their output
/// tokens and their generation spans both count, so a delegating round is
/// not inflated). Σ tokens / Σ ms over the *entire* session rather than a
/// mean of per-round rates, so one long streaming request carries the
/// weight it deserves. Untimed attempts are excluded from both sides, for
/// the same reason as in [`fmt_round_rate`]. Returns `0.0` when nothing was
/// measured, which the caller renders as `–`.
fn session_output_rate(report: &TokenSourceReport) -> f64 {
    let mut output_tokens = 0i64;
    let mut generation_ms = 0u64;
    for record in report.rows.iter().flat_map(|row| row.requests.iter()) {
        if !record.status.is_terminal()
            || record.generation_ms == 0
            || record.completion_tokens <= 0
        {
            continue;
        }
        output_tokens = output_tokens.saturating_add(record.completion_tokens.max(0));
        generation_ms = generation_ms.saturating_add(record.generation_ms);
    }
    if generation_ms == 0 || output_tokens <= 0 {
        return 0.0;
    }
    output_tokens as f64 * 1000.0 / generation_ms as f64
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
    /// Output tokens ÷ measured generation time, accumulated over only the
    /// attempts that have a per-attempt rate at all (a measured span *and*
    /// booked completion tokens — the same conditions under which the
    /// drill-in's `tok/s` column renders a number instead of `–`). An
    /// untimed attempt (a failure before any validated response, or a legacy
    /// booking with no timing) would inflate the average — its tokens would
    /// reach the numerator while its time never reaches the denominator — so
    /// it is excluded from both sides and the round renders `–` when nothing
    /// was timed.
    timed_output_tokens: i64,
    timed_generation_ms: u64,
    turns: BTreeMap<(bool, String, u32), TurnUsage>,
}

impl RoundUsage {
    fn new(number: u64) -> Self {
        Self {
            number,
            totals: UsageTotals::default(),
            timed_output_tokens: 0,
            timed_generation_ms: 0,
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
        if record.generation_ms > 0 && record.completion_tokens > 0 {
            self.timed_output_tokens = self
                .timed_output_tokens
                .saturating_add(record.completion_tokens);
            self.timed_generation_ms = self
                .timed_generation_ms
                .saturating_add(record.generation_ms);
        }
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

    /// Aggregate lifecycle of the round, derived from its turns' latest
    /// statuses. Precedence: any in-flight turn ⇒ in flight; else any failed
    /// ⇒ failed; else any interrupted/abandoned ⇒ interrupted; else completed.
    fn status(&self) -> RequestUsageStatus {
        let statuses = self.turns.values().map(|t| t.latest_status);
        let mut any_in_flight = false;
        let mut any_failed = false;
        let mut any_interrupted = false;
        let mut any_completed = false;
        for s in statuses {
            match s {
                RequestUsageStatus::InFlight => any_in_flight = true,
                RequestUsageStatus::Failed => any_failed = true,
                RequestUsageStatus::Interrupted | RequestUsageStatus::Abandoned => {
                    any_interrupted = true
                }
                RequestUsageStatus::Completed => any_completed = true,
            }
        }
        if any_in_flight {
            RequestUsageStatus::InFlight
        } else if any_failed {
            RequestUsageStatus::Failed
        } else if any_interrupted {
            RequestUsageStatus::Interrupted
        } else if any_completed {
            RequestUsageStatus::Completed
        } else {
            RequestUsageStatus::Abandoned
        }
    }
}

#[derive(Debug)]
#[allow(dead_code)] // identity fields read via FlatAttempt in the detail view
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
///
/// Only the principal actor's requests are counted here — an `envoy` tool
/// call is a forked sub-conversation whose usage belongs to the fork's own
/// context, not to this main round's usage. So envoy attempts (`actor_id !=
/// "principal"`) are dropped before aggregation, keeping the round totals,
/// the round-list table, and the detail table all principal-only.
fn usage_rounds(report: &TokenSourceReport) -> Vec<RoundUsage> {
    let mut rounds = BTreeMap::<u64, RoundUsage>::new();
    for row in &report.rows {
        // Legacy `record*` bookings (turns) and lifecycle request records can
        // coexist on one provider/model row: the snapshot merges both sources
        // and appends each terminal request's `as_turn()` to `turns`, so when
        // `requests` is non-empty those same bookings also appear in `turns`.
        // Counting the legacy turns as well would double-book them, so the
        // legacy fallback only applies to rows with no lifecycle records.
        if row.requests.is_empty() {
            for turn in &row.turns {
                rounds
                    .entry(turn.round)
                    .or_insert_with(|| RoundUsage::new(turn.round))
                    .add_legacy(turn);
            }
        } else {
            for record in &row.requests {
                if record.key.actor_id != "principal" {
                    continue;
                }
                rounds
                    .entry(record.key.round)
                    .or_insert_with(|| RoundUsage::new(record.key.round))
                    .add_record(record);
            }
        }
    }
    // Newest round first: the most recent user exchange is what the user
    // wants to see on top, mirroring the newest-first turn table inside a
    // round. With index 0 as the initial selection this also means the modal
    // opens on the latest round.
    rounds.into_values().rev().collect()
}

/// One per-attempt row in the flattened detail table: the record plus its turn
/// and attempt numbers, enough to render a `<turn>` / `<turn> - <attempt>`
/// label. Principal-only (envoy attempts are filtered upstream in
/// [`usage_rounds`]).
#[derive(Debug)]
struct FlatAttempt<'a> {
    turn: u32,
    attempt: u32,
    record: &'a RequestUsageRecord,
}

/// Collect every principal per-attempt record for `round`, newest-first. One
/// row per attempt rather than the aggregated `TurnUsage`, so a retried turn
/// shows `1st - 2nd` after `1st` instead of a collapsed `1st ×2`.
fn flat_attempts<'a>(report: &'a TokenSourceReport, round: u64) -> Vec<FlatAttempt<'a>> {
    let mut out: Vec<FlatAttempt<'a>> = report
        .rows
        .iter()
        .flat_map(|row| row.requests.iter())
        .filter(|r| r.key.round == round && r.key.actor_id == "principal")
        .map(|r| FlatAttempt {
            turn: r.key.turn,
            attempt: r.key.attempt,
            record: r,
        })
        .collect();
    // Sort by (turn, attempt) ascending, then reverse for newest-first. Ties
    // in turn are broken by attempt, so a retried turn lists attempt 2 before
    // attempt 1.
    out.sort_by_key(|a| (a.turn, a.attempt));
    out.reverse();
    out
}

/// Label for one flattened attempt row. A turn with a single attempt reads as
/// a bare ordinal ("1st"); a retried turn reads as `<turn> - <attempt>`
/// ("1st - 2nd").
fn attempt_label(turn: u32, attempt: u32) -> String {
    if attempt > 1 {
        format!("{} - {}", ordinal(turn as u64), ordinal(attempt as u64))
    } else {
        ordinal(turn as u64)
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

fn round_label(number: u64) -> String {
    if number == 0 {
        "Earlier usage".to_string()
    } else {
        format!("{} round", ordinal(number))
    }
}

/// Compact label for a row in the usage list: just the bare ordinal ("1st",
/// "2nd"), since the table header ("Usage by round") already establishes that
/// each row is a round. [`round_label`] keeps the fuller form for the detail
/// view's heading.
fn round_row_label(number: u64) -> String {
    if number == 0 {
        "Earlier".to_string()
    } else {
        ordinal(number)
    }
}

/// Compact English ordinal: 1 → "1st", 2 → "2nd", 3 → "3rd", 11/12/13 →
/// "11th"/"12th"/"13th", and so on. Used for round labels so the list reads
/// "1st round", "2nd round" rather than the heavier "Round 1".
fn ordinal(n: u64) -> String {
    let suffix = if n % 100 / 10 == 1 {
        "th"
    } else {
        match n % 10 {
            1 => "st",
            2 => "nd",
            3 => "rd",
            _ => "th",
        }
    };
    format!("{n}{suffix}")
}

/// Short state label + color for one per-attempt record.
fn attempt_state(record: &RequestUsageRecord, theme: &Theme) -> (&'static str, Style) {
    let (state, color) = match record.status {
        RequestUsageStatus::InFlight => ("in flight", theme.info()),
        RequestUsageStatus::Completed => ("completed", theme.ok()),
        RequestUsageStatus::Interrupted => ("interrupted", theme.warn()),
        RequestUsageStatus::Failed => ("failed", theme.err()),
        RequestUsageStatus::Abandoned => ("abandoned", theme.warn()),
    };
    (state, Style::default().fg(color))
}

/// Source-tint style for one per-attempt record: green when provider-reported,
/// yellow when locally estimated, plain when unknown/pending.
fn attempt_source_style(record: &RequestUsageRecord, theme: &Theme) -> Style {
    match record.source {
        RequestUsageSource::Reported => Style::default().fg(theme.ok()),
        RequestUsageSource::Estimated => Style::default().fg(theme.warn()),
        RequestUsageSource::Unknown => Style::default().fg(theme.fg()),
    }
}

/// Short label for a round's aggregate lifecycle, for the round-list table.
fn round_state(round: &RoundUsage) -> &'static str {
    match round.status() {
        RequestUsageStatus::InFlight => "in flight",
        RequestUsageStatus::Completed => "done",
        RequestUsageStatus::Interrupted => "interrupted",
        RequestUsageStatus::Failed => "failed",
        RequestUsageStatus::Abandoned => "abandoned",
    }
}

/// Round lifecycle label + color, ready to drop into a table cell.
fn round_state_styled(round: &RoundUsage, theme: &Theme) -> (&'static str, Style) {
    let color = match round.status() {
        RequestUsageStatus::InFlight => theme.info(),
        RequestUsageStatus::Completed => theme.ok(),
        RequestUsageStatus::Interrupted | RequestUsageStatus::Abandoned => theme.warn(),
        RequestUsageStatus::Failed => theme.err(),
    };
    (round_state(round), Style::default().fg(color))
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

/// Left-align `text` into a fixed display-`width` field, padding the trailing
/// cells with spaces. Width-aware (uses [`UnicodeWidthStr`]) so wide glyphs
/// don't corrupt column alignment. Returns the text unchanged when it already
/// meets or exceeds `width` (the caller is responsible for truncating first).
fn format_padded_left(text: &str, width: usize) -> String {
    let text_width = text.width();
    if text_width >= width {
        text.to_string()
    } else {
        let mut out = text.to_string();
        out.push_str(&" ".repeat(width - text_width));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use muta_contracts::ContextTokenSource;

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
        let (body, _follow) = list_body(
            &report,
            Some(ContextTokenSnapshot {
                tokens: 12_500,
                source: ContextTokenSource::Projection,
                overhead_tokens: Some(3_200),
                history_tokens: Some(9_300),
            }),
            200_000,
            300,
            0,
            80,
            &theme,
        );
        let text = body_text(&body);
        let size = body
            .iter()
            .flat_map(|line| &line.spans)
            .find(|span| span.content.contains("12.5k (+300) / 200.0k"))
            .expect("context size span");

        assert!(text.contains("Base overhead"));
        assert!(text.contains("3.2k"));
        assert!(text.contains("Conversation"));
        assert!(text.contains("9.3k"));
        assert!(text.contains("Draft prompt"));
        assert!(text.contains("300"));

        // The in-body section headings ("Current AI-visible context",
        // "Request usage") have been removed — the modal title already
        // conveys this, so the subtitles were redundant noise.
        assert!(!text.contains("Current AI-visible context"));
        assert!(!text.contains("Request usage"));
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
        let ledger = muta_contracts::TokenSourceLedger::new();

        let first = ledger.begin_request("session", "relay", "model-a", 2, 1, 800);
        ledger.settle_request(&first, RequestUsageStatus::Interrupted, None, 20, 0);
        let retry = ledger.begin_request("session", "relay", "model-a", 2, 1, 800);
        ledger.settle_request(
            &retry,
            RequestUsageStatus::Completed,
            Some(muta_contracts::TokenUsage {
                prompt_tokens: 790,
                completion_tokens: 40,
                total_tokens: 830,
                ..Default::default()
            }),
            0,
            2_000,
        );
        let second_turn =
            ledger.begin_request("session", "another-provider", "model-b", 2, 2, 1_200);
        ledger.settle_request(&second_turn, RequestUsageStatus::Completed, None, 60, 0);
        let next_round = ledger.begin_request("session", "relay", "model-a", 3, 1, 1_500);
        ledger.settle_request(&next_round, RequestUsageStatus::Completed, None, 75, 0);
        let report = ledger.snapshot_for_session("session");

        assert_eq!(token_report_round_count(&report), 2);
        let (list_body_lines, follow) = list_body(&report, None, 0, 0, 0, 80, &theme);
        let list = body_text(&list_body_lines);
        // List rows use bare ordinals ("3rd", "2nd"); the round context is
        // carried by the table header, and there is no longer a "Usage by
        // round" sub-heading (it was redundant with the modal title).
        // The list shows both rounds, newest (3rd) on top.
        assert!(list.contains("2nd"));
        assert!(list.contains("3rd"));
        assert!(!list.contains("Usage by round"));
        assert!(!list.contains("2nd round")); // that fuller form is detail-only
        assert!(!list.contains("Round 2"));
        assert!(!list.contains("relay"));
        assert!(!list.contains("model-a"));
        assert!(!list.contains("Provider-reported"));
        // The Total row was removed (it duplicated the context-size summary),
        // and so were the closing rule and the hint line.
        assert!(!list.contains("Total"));
        assert!(!list.contains("inspect its model turns"));
        // The Turns column was replaced by the round's average TPS (the turn
        // count lives in the drill-in), and a dedicated State column carries
        // the lifecycle signal (no "…" / "›" suffix glyphs).
        assert!(!list.contains('›'));
        assert!(!list.contains("Turns"));
        assert!(list.contains("State"));
        assert!(list.contains("TPS"));
        assert!(list.contains("done"));
        // "Output rate" is now the *session-wide* average, derived from the
        // ledger: the only timed attempt is round 2's retry (40 output tokens
        // over 2 s) → 20.0 tok/s. The untimed attempts contribute tokens but
        // no time, so they cannot drag the average.
        assert!(list.contains("Output rate"));
        assert!(list.contains("20.0 tok/s"));
        // Per-round TPS cells: round 2's timed retry → "20"; round 3 (75
        // estimated tokens, untimed) → "–". Find them on their own rows so a
        // substring collision can't fake a pass.
        let rate_cell = |label: &str| {
            list_body_lines
                .iter()
                .find(|line| line.spans.iter().any(|s| s.content.trim() == label))
                .and_then(|line| line.spans.last())
                .map(|s| s.content.trim().to_string())
                .unwrap_or_else(|| panic!("no TPS cell on the {label} round row"))
        };
        assert_eq!(rate_cell("3rd"), "–");
        assert_eq!(rate_cell("2nd"), "20");
        // Rounds are newest-first: the latest (3rd) round is row 0, so the
        // modal opens on the most recent user exchange.
        let pos_3rd = list.find("3rd").expect("3rd round label");
        let pos_2nd = list.find("2nd").expect("2nd round label");
        assert!(
            pos_3rd < pos_2nd,
            "rounds must be newest-first (3rd before 2nd): got 3rd@{pos_3rd} vs 2nd@{pos_2nd}"
        );
        // `selected == 0` selects the first (newest) round row; the follow
        // index must point at that row (the one carrying "3rd") for
        // auto-scroll.
        let follow_idx = follow.expect("a follow index for the selected row");
        assert!(
            list_body_lines[follow_idx]
                .spans
                .iter()
                .any(|span| span.content.contains("3rd")),
            "follow index {follow_idx} does not point at the selected round row"
        );

        // Drill into round 2 — index 1 now that rounds are newest-first
        // (3rd at index 0).
        let detail = detail_body(&report, 1, 80, &theme);
        let detail_text = body_text(&detail);
        // The table is flattened: one row per attempt, principal-only. Turn 1
        // was retried (attempt 1 interrupted, attempt 2 completed). A turn's
        // first attempt shows a bare ordinal; later attempts show
        // "<turn> - <attempt>".
        assert!(detail_text.contains("2nd"));
        assert!(detail_text.contains("1st - 2nd"));
        assert!(detail_text.contains("1st"));
        assert!(!detail_text.contains("×2"));
        assert!(!detail_text.contains("1st - 1st"));
        assert!(detail_text.contains("2 / 3"));
        assert!(!detail_text.contains("another-provider"));
        assert!(!detail_text.contains("Provider-reported"));
        // The token-source legend is rendered beneath the turns table.
        assert!(detail_text.contains("green"));
        assert!(detail_text.contains("local estimate"));
        // The detail table carries a per-attempt output-rate column. The
        // retried attempt settled with 40 completion tokens over a 2 s
        // measured generation span → "20" tok/s; the untimed attempt (an
        // interrupt with no measured span) and the legacy-estimated turn
        // render a dash instead.
        assert!(detail_text.contains("tok/s"));
        let rate_for = |turn_label: &str| -> String {
            detail
                .iter()
                .find(|line| line.spans.iter().any(|s| s.content.trim() == turn_label))
                .and_then(|line| line.spans.last())
                .map(|s| s.content.trim().to_string())
                .unwrap_or_else(|| panic!("no rate cell for turn {turn_label}"))
        };
        assert_eq!(rate_for("1st - 2nd"), "20");
        assert_eq!(rate_for("1st"), "–");
        assert_eq!(rate_for("2nd"), "–");
        // The detail page's title is now the breadcrumb, not an in-body
        // "2nd round" heading.
        assert!(!detail_text.contains("2nd round"));
        // No in-table separator rule beneath the Turns header.
        assert!(
            !detail.iter().any(|line| line.spans.iter().any(|s| s
                .content
                .chars()
                .all(|c| c == '─')
                && !s.content.is_empty())),
            "turns table must not carry a separator rule"
        );

        // Round total is rendered as plain foreground now (no provenance
        // color/underline encoding).
        let round_total = detail
            .iter()
            .flat_map(|line| &line.spans)
            .find(|span| span.content.trim() == "2.9k")
            .expect("round total span");
        assert!(round_total.style.add.is_empty());
    }

    #[test]
    fn selected_round_row_uses_background_highlight_not_arrow_or_bold() {
        let theme = Theme::default();
        let ledger = muta_contracts::TokenSourceLedger::new();
        let a = ledger.begin_request("session", "relay", "model-a", 2, 1, 800);
        ledger.settle_request(&a, RequestUsageStatus::Completed, None, 40, 0);
        let b = ledger.begin_request("session", "relay", "model-a", 3, 1, 1_500);
        ledger.settle_request(&b, RequestUsageStatus::Completed, None, 75, 0);
        let report = ledger.snapshot_for_session("session");

        // Select the second round (index 1).
        let (body, follow) = list_body(&report, None, 0, 0, 1, 80, &theme);
        let selected_line = follow.expect("a follow index for the selected row");
        let line = &body[selected_line];

        // No ">" arrow marker anywhere in the selected row.
        assert!(
            !line.spans.iter().any(|span| span.content.contains('>')),
            "selected row must not carry an arrow marker"
        );
        // Every span of the selected row carries the selection background.
        for span in &line.spans {
            assert_eq!(
                span.style.bg,
                theme.selected(),
                "span {:?} not using selection background",
                span.content
            );
        }
        // And no bold text on the selected label (the old brand+bold style).
        // The row renders as [indent, label, gutter, tokens, turns, trail].
        let label_span = &line.spans[1];
        assert!(
            !label_span.style.add.contains(Modifier::BOLD),
            "selected label must not be bold"
        );
    }

    #[test]
    fn round_labels_use_ordinal_form() {
        // `round_label` keeps the fuller "1st round" form for the detail view.
        assert_eq!(round_label(0), "Earlier usage");
        assert_eq!(round_label(1), "1st round");
        assert_eq!(round_label(2), "2nd round");
        assert_eq!(round_label(3), "3rd round");
        assert_eq!(round_label(4), "4th round");
        assert_eq!(round_label(11), "11th round");
        assert_eq!(round_label(12), "12th round");
        assert_eq!(round_label(13), "13th round");
        assert_eq!(round_label(21), "21st round");
        assert_eq!(round_label(22), "22nd round");
        assert_eq!(round_label(23), "23rd round");
        assert_eq!(round_label(111), "111th round");
        assert_eq!(round_label(112), "112th round");

        // `round_row_label` is the bare ordinal used in the usage list, where
        // the "Usage by round" heading carries the round context.
        assert_eq!(round_row_label(0), "Earlier");
        assert_eq!(round_row_label(1), "1st");
        assert_eq!(round_row_label(2), "2nd");
        assert_eq!(round_row_label(3), "3rd");
        assert_eq!(round_row_label(11), "11th");
        assert_eq!(round_row_label(13), "13th");
        assert_eq!(round_row_label(21), "21st");
        assert_eq!(round_row_label(112), "112th");
    }

    /// The round table has four columns (Round / State / Tokens / TPS),
    /// each content-sized, with leftover width split evenly across the
    /// inter-column gaps. The token column must align across the header and
    /// every data row, and each row fills the full body width (selection band
    /// integrity).
    #[test]
    fn round_columns_are_content_sized_and_aligned() {
        let theme = Theme::default();
        let ledger = muta_contracts::TokenSourceLedger::new();
        let a = ledger.begin_request("session", "relay", "model-a", 2, 1, 800);
        ledger.settle_request(&a, RequestUsageStatus::Completed, None, 40, 0);
        // A second round with a longer token total so the Tokens column must
        // grow to fit it.
        let b = ledger.begin_request("session", "relay", "model-a", 3, 1, 0);
        ledger.settle_request(&b, RequestUsageStatus::Completed, None, 1_234_567, 0);
        let report = ledger.snapshot_for_session("session");

        let body_width = 80usize;
        let (body, _follow) = list_body(&report, None, 0, 0, 0, body_width, &theme);

        // Header row carries "Tokens"; data rows carry a bare ordinal ("3rd",
        // "2nd") plus a token value.
        let header = body
            .iter()
            .find(|line| {
                line.spans
                    .iter()
                    .any(|span| span.content.trim() == "Tokens")
            })
            .expect("header row");
        let is_ordinal = |c: &str| -> bool {
            let b = c.as_bytes();
            b.len() >= 3
                && b[..b.len() - 2].iter().all(|x| x.is_ascii_digit())
                && matches!(&b[b.len() - 2..], b"st" | b"nd" | b"rd" | b"th")
        };
        let data_rows: Vec<&Line> = body
            .iter()
            .filter(|line| {
                let has_ordinal = line
                    .spans
                    .iter()
                    .any(|span| is_ordinal(span.content.trim()));
                let has_number = line.spans.iter().any(|span| {
                    let c = span.content.trim();
                    !c.is_empty()
                        && c.chars()
                            .all(|ch| ch.is_ascii_digit() || ".kMB—".contains(ch))
                });
                has_ordinal && has_number
            })
            .collect();
        assert_eq!(data_rows.len(), 2, "expected two round data rows");

        // Span layout is identical for header and data rows:
        //   [indent, label, gap0, state, gap1, tokens, gap2, rate]
        // so the token value lives at span index 5 in every row. Its leading
        // column offset is the cumulative width of spans 0..5.
        const TOKEN_SPAN_IDX: usize = 5;
        let token_offset = |line: &Line| -> usize {
            line.spans[..TOKEN_SPAN_IDX]
                .iter()
                .map(|s| s.content.width())
                .sum()
        };
        let header_off = token_offset(header);
        let offsets: Vec<usize> = data_rows.iter().map(|row| token_offset(row)).collect();
        for off in &offsets {
            assert_eq!(
                *off, header_off,
                "token column not aligned across rows (header={header_off}, row={off})"
            );
        }

        // Every data row fills the full body width (selection band integrity):
        // the sum of span display widths equals body_width.
        for row in &data_rows {
            let total: usize = row.spans.iter().map(|s| s.content.width()).sum();
            assert_eq!(
                total, body_width,
                "data row does not fill body width ({total} != {body_width}); \
                 selection highlight would not span the full row"
            );
        }
    }

    /// The detail view's turn table must (a) render newest-first and (b) tint
    /// token totals by their provenance: green for provider-reported, yellow
    /// for local estimate, plain for mixed/pending. Turns are newest-first.
    #[test]
    fn detail_turns_are_newest_first_and_color_coded_by_source() {
        let theme = Theme::default();
        let ledger = muta_contracts::TokenSourceLedger::new();

        // Turn 1: reported usage (green), 40 completion tokens over a 2 s
        // measured generation span → 20 tok/s. Turn 2: estimated (yellow)
        // with no measured span → the rate cell renders a dash.
        let t1 = ledger.begin_request("session", "relay", "model-a", 2, 1, 0);
        ledger.settle_request(
            &t1,
            RequestUsageStatus::Completed,
            Some(muta_contracts::TokenUsage {
                prompt_tokens: 100,
                completion_tokens: 40,
                total_tokens: 140,
                ..Default::default()
            }),
            0,
            2_000,
        );
        let t2 = ledger.begin_request("session", "relay", "model-a", 2, 2, 0);
        ledger.settle_request(&t2, RequestUsageStatus::Completed, None, 60, 0);
        let report = ledger.snapshot_for_session("session");

        let detail = detail_body(&report, 0, 80, &theme);
        let detail_text = body_text(&detail);

        // Newest-first: turn 2 appears before turn 1. Both have a single
        // attempt, so they show as bare ordinals ("2nd", "1st").
        let pos_2nd = detail_text.find("2nd").expect("2nd turn label");
        let pos_1st = detail_text.find("1st").expect("1st turn label");
        assert!(
            pos_2nd < pos_1st,
            "turns must be newest-first (2nd before 1st): got 2nd@{pos_2nd} vs 1st@{pos_1st}"
        );

        // The estimated turn's total (60) is tinted warning-yellow; the
        // reported turn's total (140) is tinted success-green. Find the spans.
        let find_total_span = |needle: &str| {
            detail
                .iter()
                .flat_map(|line| &line.spans)
                .find(|span| span.content.trim() == needle)
                .unwrap_or_else(|| panic!("token span {needle:?} not found"))
        };
        let reported_total = find_total_span("140");
        assert_eq!(
            reported_total.style.fg,
            theme.ok(),
            "reported turn total must be green (success)"
        );
        let estimated_total = find_total_span("60");
        assert_eq!(
            estimated_total.style.fg,
            theme.warn(),
            "estimated turn total must be yellow (warning)"
        );

        // The legend line explains both colors.
        assert!(detail_text.contains("green"));
        assert!(detail_text.contains("provider-reported"));
        assert!(detail_text.contains("yellow"));
        assert!(detail_text.contains("local estimate"));

        // The per-attempt rate column: the reported turn shows its settled
        // output rate (40 completion tokens / 2 s = 20 tok/s), the untimed
        // estimated turn shows a dash.
        let rate_cell = |turn_label: &str| {
            detail
                .iter()
                .find(|line| line.spans.iter().any(|s| s.content.trim() == turn_label))
                .and_then(|line| line.spans.last())
                .map(|s| s.content.trim().to_string())
                .unwrap_or_else(|| panic!("no rate cell for turn {turn_label}"))
        };
        assert_eq!(rate_cell("1st"), "20");
        assert_eq!(rate_cell("2nd"), "–");
    }

    /// The detail turns table degrades in whole column groups as the modal
    /// narrows — the Turn label flexes, but inter-column gutters never
    /// squeeze. Dropping below a tier boundary removes the entire column
    /// (header and every row together) in one step, and the remaining
    /// right-aligned columns keep hugging the right modal margin.
    #[test]
    fn detail_columns_degrade_atomically_with_width() {
        let theme = Theme::default();
        let ledger = muta_contracts::TokenSourceLedger::new();
        let t = ledger.begin_request("session", "relay", "model-a", 2, 1, 0);
        ledger.settle_request(
            &t,
            RequestUsageStatus::Completed,
            Some(muta_contracts::TokenUsage {
                prompt_tokens: 100,
                completion_tokens: 40,
                total_tokens: 140,
                ..Default::default()
            }),
            0,
            2_000,
        );
        let report = ledger.snapshot_for_session("session");

        // The table header is the line whose first span trims to "Turn".
        let header_at = |width: usize| -> String {
            detail_body(&report, 0, width, &theme)
                .into_iter()
                .find(|line| {
                    line.spans
                        .first()
                        .is_some_and(|s| s.content.trim() == "Turn")
                })
                .map(|line| {
                    line.spans
                        .iter()
                        .map(|s| s.content.as_ref())
                        .collect::<String>()
                })
                .expect("table header row")
        };

        // ≥62: full split. The label column (not the gutters) absorbs slack:
        // at 62 exactly the label sits at its 6-cell floor.
        let wide = header_at(62);
        for title in ["Input", "Output", "Total", "tok/s"] {
            assert!(
                wide.contains(title),
                "62-col header missing {title:?}; header = {wide:?}"
            );
        }
        assert_eq!(
            wide.find("State"),
            Some(6),
            "label must be at its floor at 62"
        );
        // Header and every row fill the body width exactly.
        let wide_rows = detail_body(&report, 0, 62, &theme);
        for line in wide_rows.iter().filter(|l| {
            l.spans
                .iter()
                .any(|s| s.content.trim() == "1st" || s.content.trim() == "Turn")
        }) {
            let w: usize = line.spans.iter().map(|s| s.content.width()).sum();
            assert_eq!(w, 62, "row must fill body width exactly");
        }

        // 61: the Input+Output pair drops as one atom — never half a pair,
        // never a squeezed gutter.
        let compact = header_at(61);
        assert!(!compact.contains("Input"));
        assert!(!compact.contains("Output"));
        assert!(compact.contains("Tokens"));
        assert!(compact.contains("tok/s"));

        // 41: the Tokens column drops; tok/s survives alone and still hugs
        // the right edge.
        let rate_only = header_at(41);
        assert!(!rate_only.contains("Tokens"), "rate_only = {rate_only:?}");
        assert!(rate_only.contains("tok/s"));
        assert!(
            rate_only.ends_with("tok/s"),
            "tok/s must hug the right margin: {rate_only:?}"
        );

        // 31: every value column is gone as a unit; only label + state remain.
        let bare = header_at(31);
        assert!(!bare.contains("tok/s"));
        assert!(bare.trim_end().ends_with("State"));

        // Tier boundaries agree with the width formula.
        assert_eq!(AttemptColumns::for_width(62), AttemptColumns::Split);
        assert_eq!(AttemptColumns::for_width(61), AttemptColumns::Total);
        assert_eq!(AttemptColumns::for_width(42), AttemptColumns::Total);
        assert_eq!(AttemptColumns::for_width(41), AttemptColumns::Rate);
        assert_eq!(AttemptColumns::for_width(32), AttemptColumns::Rate);
        assert_eq!(AttemptColumns::for_width(31), AttemptColumns::State);
    }

    /// The provenance legend is responsive: the full single-line form when it
    /// fits, the shortened wording when it doesn't, and a two-line fallback
    /// for very narrow modals — the explanation is never clipped mid-word.
    #[test]
    fn provenance_legend_collapses_gracefully_on_narrow_modals() {
        let theme = Theme::default();

        let width = |lines: &[Vec<Span<'static>>]| -> Vec<usize> {
            lines.iter().map(|spans| spans_width(spans)).collect()
        };
        let text = |lines: &[Vec<Span<'static>>]| -> String {
            lines
                .iter()
                .map(|spans| spans.iter().map(|s| s.content.as_ref()).collect::<String>())
                .collect::<Vec<_>>()
                .join("\n")
        };

        // Wide enough → the full single-line form, exactly one line.
        let full = legend_lines(80, &theme);
        assert_eq!(full.len(), 1);
        assert!(text(&full).contains("green = provider-reported"));
        assert!(text(&full).contains("yellow = local estimate"));

        // One column below the full form's width → shortened single line.
        let full_w = width(&full)[0];
        let short = legend_lines(full_w - 1, &theme);
        assert_eq!(short.len(), 1);
        assert!(text(&short).contains("green reported"));
        assert!(text(&short).contains("yellow estimated"));
        assert!(!text(&short).contains("provider-reported"));

        // Below the short form → two lines, each fitting the width.
        let short_w = width(&short)[0];
        let split = legend_lines(short_w - 1, &theme);
        assert_eq!(split.len(), 2);
        assert!(text(&split).contains("green reported"));
        assert!(text(&split).contains("yellow estimated"));
        for w in width(&split) {
            assert!(w < short_w, "split line width {w} must fit < {short_w}");
        }
    }

    /// Envoy sub-conversations are forks: their usage belongs to the fork's own
    /// context, so the round usage view is principal-only. Envoy attempts must
    /// not appear in the detail table, and their tokens must not leak into the
    /// round's totals.
    #[test]
    fn detail_excludes_envoy_attempts_and_totals() {
        let theme = Theme::default();
        let ledger = muta_contracts::TokenSourceLedger::new();

        // Principal turns 1 and 2.
        let p1 = ledger.begin_request("session", "relay", "model-a", 2, 1, 0);
        ledger.settle_request(&p1, RequestUsageStatus::Completed, None, 40, 0);
        let p2 = ledger.begin_request("session", "relay", "model-a", 2, 2, 0);
        ledger.settle_request(&p2, RequestUsageStatus::Completed, None, 60, 0);
        // An envoy sub-turn (turn 1 of the envoy actor) in the same round,
        // with its own token spend that must NOT count toward the round.
        let e1 = ledger.begin_request_for_actor(
            "session",
            "envoy:call_xyz",
            "relay",
            "model-a",
            2,
            1,
            0,
        );
        ledger.settle_request(&e1, RequestUsageStatus::Completed, None, 120, 0);
        let report = ledger.snapshot_for_session("session");

        let detail = detail_body(&report, 0, 80, &theme);
        let detail_text = body_text(&detail);

        // No envoy section, no envoy attempts, no arrow glyph.
        assert!(!detail_text.contains("Envoy"));
        assert!(!detail_text.contains('↳'));
        // The envoy's token spend (120) does not appear — the round total is
        // the principal-only sum (40 + 60 = 100).
        assert!(!detail_text.contains("120"));
        assert!(detail_text.contains("100"));
        // Principal turns are the only rows; newest-first as bare ordinals.
        let pos_2nd = detail_text.find("2nd").expect("2nd turn");
        let pos_1st = detail_text.find("1st").expect("1st turn");
        assert!(pos_2nd < pos_1st, "principal turns newest-first");
    }

    /// Throughput is shown at three scopes — per attempt (the drill-in's
    /// `tok/s` column), per round (the list's `TPS` column), and per session
    /// (the `Output rate` row) — and all three divide the same quantities, so
    /// they must agree: a round's TPS is the time-weighted mean of its
    /// attempts' rates, and the session rate is the time-weighted mean across
    /// rounds. Untimed attempts (a failure before any validated response, a
    /// legacy booking, an in-flight record) are excluded from both sides of
    /// every quotient rather than inflating it, and a scope with nothing
    /// timed renders `–`.
    #[test]
    fn throughput_agrees_across_attempt_round_and_session_scopes() {
        let theme = Theme::default();
        let ledger = muta_contracts::TokenSourceLedger::new();

        // Round 2: two timed attempts — 60 output tokens over 1 s (60 tok/s)
        // and 40 output tokens over 2 s (20 tok/s). The round's TPS must be
        // the *time-weighted* mean (100 tokens / 3 s ≈ 33.3), not the plain
        // mean of the two rates (40).
        let r2a = ledger.begin_request("session", "relay", "model-a", 2, 1, 0);
        ledger.settle_request(
            &r2a,
            RequestUsageStatus::Completed,
            Some(muta_contracts::TokenUsage {
                prompt_tokens: 100,
                completion_tokens: 60,
                total_tokens: 160,
                ..Default::default()
            }),
            0,
            1_000,
        );
        let r2b = ledger.begin_request("session", "relay", "model-a", 2, 2, 0);
        ledger.settle_request(
            &r2b,
            RequestUsageStatus::Completed,
            Some(muta_contracts::TokenUsage {
                prompt_tokens: 200,
                completion_tokens: 40,
                total_tokens: 240,
                ..Default::default()
            }),
            0,
            2_000,
        );
        // Round 3: untimed (interrupted before a validated response, 500
        // estimated tokens booked but no measured span) → its TPS is `–` and
        // it contributes to neither side of the session average.
        let r3 = ledger.begin_request("session", "relay", "model-a", 3, 1, 0);
        ledger.settle_request(&r3, RequestUsageStatus::Interrupted, None, 500, 0);
        let report = ledger.snapshot_for_session("session");

        let (list_lines, _follow) = list_body(&report, None, 0, 0, 0, 80, &theme);
        let list = body_text(&list_lines);
        let tps_cell = |label: &str| {
            list_lines
                .iter()
                .find(|line| line.spans.iter().any(|s| s.content.trim() == label))
                .and_then(|line| line.spans.last())
                .map(|s| s.content.trim().to_string())
                .unwrap_or_else(|| panic!("no TPS cell on the {label} round row"))
        };
        // Round 2: 100 tokens / 3 s → 33.3, rendered in the column's integer
        // form ("33", one decimal is reserved for sub-10 rates). Round 3:
        // nothing timed → "–".
        assert_eq!(tps_cell("2nd"), "33");
        assert_eq!(tps_cell("3rd"), "–");
        // Session average spans exactly round 2's timed attempts: 100 tokens
        // / 3 s → 33.3 tok/s, always one decimal on this row.
        assert!(list.contains("Output rate"));
        assert!(list.contains("33.3 tok/s"), "list was: {list}");

        // The drill-in's per-attempt rates aggregate to the round's cell.
        let detail = detail_body(&report, 0, 80, &theme); // newest round = 3rd
        assert!(
            body_text(&detail).contains("500"),
            "the untimed round still shows its token totals"
        );
        let detail = detail_body(&report, 1, 80, &theme); // round 2
        let rate_for = |turn_label: &str| {
            detail
                .iter()
                .find(|line| line.spans.iter().any(|s| s.content.trim() == turn_label))
                .and_then(|line| line.spans.last())
                .map(|s| s.content.trim().to_string())
                .unwrap_or_else(|| panic!("no rate cell for turn {turn_label}"))
        };
        assert_eq!(rate_for("2nd"), "20");
        assert_eq!(rate_for("1st"), "60");

        // A report with nothing timed at all: the session row renders the
        // dash rather than a fabricated (or infinite) figure.
        let empty = muta_contracts::TokenSourceLedger::new();
        let only_untimed = empty.begin_request("session", "relay", "model-a", 1, 1, 0);
        empty.settle_request(&only_untimed, RequestUsageStatus::Completed, None, 80, 0);
        let report = empty.snapshot_for_session("session");
        let (lines, _) = list_body(&report, None, 0, 0, 0, 80, &theme);
        let session_rate_span = lines
            .iter()
            .find(|line| line.spans.iter().any(|s| s.content.trim() == "Output rate"))
            .and_then(|line| line.spans.last())
            .expect("Output rate row with a value span");
        assert_eq!(session_rate_span.content.trim(), "–");
    }

    /// A row that mixes legacy `record*` bookings (turns) with lifecycle
    /// request records must not double-count: the snapshot already appends
    /// each terminal request's `as_turn()` to `turns`, so the legacy fallback
    /// only applies to rows with no lifecycle records at all.
    #[test]
    fn mixed_legacy_and_request_rows_do_not_double_count() {
        let ledger = muta_contracts::TokenSourceLedger::new();
        // Legacy booking on the same provider/model row: 100 tokens.
        ledger.record_turn(
            "relay",
            "model-a",
            muta_contracts::TokenTurn {
                round: 1,
                turn: 1,
                reported: true,
                prompt_tokens: 70,
                completion_tokens: 30,
                total_tokens: 100,
                ..Default::default()
            },
        );
        // Lifecycle booking on the same row: 200 tokens.
        let req = ledger.begin_request("session", "relay", "model-a", 2, 1, 0);
        ledger.settle_request(
            &req,
            RequestUsageStatus::Completed,
            Some(muta_contracts::TokenUsage {
                prompt_tokens: 150,
                completion_tokens: 50,
                total_tokens: 200,
                ..Default::default()
            }),
            0,
            0,
        );
        let report = ledger.snapshot_for_session("session");
        // Sanity: the row really is mixed (both sources populated).
        let row = report
            .rows
            .iter()
            .find(|r| r.provider == "relay" && r.model == "model-a")
            .expect("mixed row");
        assert!(!row.turns.is_empty() && !row.requests.is_empty());

        let rounds = usage_rounds(&report);
        // Round 2 must hold only the request's 200 tokens, not 200 + the
        // as_turn() copy of it that also landed in `turns`.
        let round2 = rounds
            .iter()
            .find(|r| r.number == 2)
            .expect("round 2 usage");
        assert_eq!(
            round2.totals.total(),
            200,
            "lifecycle round must not double-count its legacy turn copy"
        );
    }
}
