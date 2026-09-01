//! The Connections modal (provider instance management) and connection inspection details.

use mutx_engine::{
    Frame, {Line, Span}, {Modifier, Style},
};

use super::super::common::truncate_ellipsis;
use super::common::{
    draw_picker_search_row, match_set, place_picker_search_cursor, search_empty_body,
    split_search_body,
};
use crate::components::options::{ChoiceTone, choice_style};
use crate::components::row::{GUTTER, ListRow, RowGroup, RowStyledAtom};
use crate::model::layout::LayoutMap;
use crate::model::selection::SelectionState;
use crate::primitives::{
    BodyRenderOptions, FixedModalSpec, FooterHint, FooterHintWithBand, SCROLL_EDGE_MARGIN,
    breadcrumb_parts, keymap_body_lines, keymap_page_footer_hints, keyvocab, modal_area,
    modal_frame, modal_header, modal_header_parts, render_body, render_centered_body,
    render_modal_footer, render_modal_footer_with_more,
};
use crate::providers::RankedProvider;
use crate::view::Theme;

/// Draw the **Connections** modal — the provider-instance management surface (`/connections`).
#[allow(clippy::too_many_arguments)]
pub fn draw_connections_modal(
    frame: &mut Frame,
    layout_map: &mut LayoutMap,
    providers: &[RankedProvider],
    current_provider: &str,
    modal_index: usize,
    query: &str,
    cursor_position: usize,
    scroll: &mut usize,
    follow_selection: bool,
    search: bool,
    keymap_open: bool,
    theme: &Theme,
    selection: &SelectionState,
    connection_info_detail: bool,
    connection_detail: Option<&muta_contracts::ConnectionDetail>,
    connection_info_scroll: &mut usize,
    spinner_phase: usize,
    connection_info_standalone: bool,
) -> mutx_engine::Rect {
    let area = modal_area(frame, FixedModalSpec::PROVIDER);
    let f = modal_frame(frame, area, theme.panel(), true, true);

    let header_rect = f.header;

    // `a add` opens the preset chooser and `Enter details` drills into connection info/usage.
    let browse_hints: [FooterHint; 8] = [
        FooterHint::navigation(keyvocab::ARROWS_UD, "navigate"),
        FooterHint::secondary("/", "search"),
        FooterHint::primary(keyvocab::ENTER, "details"),
        FooterHint::secondary("a", "preset"),
        FooterHint::secondary("c", "custom"),
        FooterHint::secondary("e", "edit"),
        FooterHint::secondary("r", "refresh"),
        FooterHint::always(keyvocab::ESC, "close"),
    ];
    let browse_extra: [FooterHintWithBand; 1] = [FooterHintWithBand {
        key: "D",
        label: "delete",
        rank: 70,
    }];
    let search_hints: [FooterHint; 3] = [
        FooterHint::secondary("type", "filter"),
        FooterHint::navigation(keyvocab::ARROWS_UD, "navigate"),
        FooterHint::always(keyvocab::ESC, "clear search"),
    ];
    let (hints, extra): (&[FooterHint], &[FooterHintWithBand]) = if search {
        (&search_hints, &[])
    } else {
        (&browse_hints, &browse_extra)
    };

    if keymap_open {
        modal_header(
            frame,
            header_rect,
            &format!("Connections{}keybindings", crate::design::JOIN_BREADCRUMB),
            theme,
        );
        let body = keymap_body_lines(hints, extra, theme);
        let rows: Vec<crate::components::selectable_body::SelectableRow> = body
            .into_iter()
            .map(crate::components::selectable_body::SelectableRow::from_line)
            .collect();
        crate::components::selectable_body::render_selectable_body(
            frame, f.body, &rows, scroll, None, theme, selection, layout_map,
        );
        if let Some(fo) = f.footer {
            render_modal_footer(frame, fo, &keymap_page_footer_hints(), theme);
        }
        return area;
    }

    if connection_info_detail {
        if connection_info_standalone {
            let conn_title = connection_detail
                .map(|d| format!("Connection Details [{}]", d.name))
                .unwrap_or_else(|| "Connection Details".to_string());
            modal_header(frame, f.header, &conn_title, theme);
        } else {
            let conn_title = connection_detail
                .map(|d| format!("Details [{}]", d.name))
                .unwrap_or_else(|| "Details".to_string());
            let header = breadcrumb_parts("Connections", &conn_title);
            modal_header_parts(frame, f.header, &header, theme);
        }
        let detail_footer: [FooterHint; 3] = if connection_info_standalone {
            [
                FooterHint::always(keyvocab::ESC, "close"),
                FooterHint::secondary("r", "refresh"),
                FooterHint::secondary("e", "edit"),
            ]
        } else {
            [
                FooterHint::always(keyvocab::ESC, "list"),
                FooterHint::secondary("r", "refresh"),
                FooterHint::secondary("e", "edit"),
            ]
        };
        let body = match connection_detail {
            None => {
                const SPINNER_FRAMES: [&str; 10] =
                    ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
                let spin = SPINNER_FRAMES[spinner_phase % SPINNER_FRAMES.len()];
                vec![
                    Line::from(""),
                    Line::from(vec![
                        Span::styled(format!("{spin} "), Style::default().fg(theme.primary)),
                        Span::styled(
                            "Loading connection details and provider usage…",
                            Style::default().fg(theme.muted()),
                        ),
                    ]),
                ]
            }
            Some(detail) => connection_detail_body(detail, spinner_phase, theme),
        };
        let rows: Vec<crate::components::selectable_body::SelectableRow> = body
            .into_iter()
            .map(crate::components::selectable_body::SelectableRow::from_line)
            .collect();
        crate::components::selectable_body::render_selectable_body(
            frame,
            f.body,
            &rows,
            connection_info_scroll,
            None,
            theme,
            selection,
            layout_map,
        );
        if let Some(fo) = f.footer {
            render_modal_footer(frame, fo, &detail_footer, theme);
        }
        return area;
    }

    modal_header(frame, header_rect, "Connections", theme);

    let (search_rect, body_rect) = split_search_body(f.body, search);
    if let Some(search_rect) = search_rect {
        draw_picker_search_row(frame, search_rect, query, cursor_position, theme);
    }

    if providers.is_empty() && !search {
        let body = connections_empty_body(theme);
        render_centered_body(frame, body_rect, body);
        if let Some(fo) = f.footer {
            render_modal_footer_with_more(frame, fo, hints, extra, theme);
        }
        return area;
    }

    if providers.is_empty() && search {
        let body = search_empty_body(theme);
        render_centered_body(frame, body_rect, body);
        if let Some(fo) = f.footer {
            render_modal_footer_with_more(frame, fo, hints, extra, theme);
        }
        if let Some(sr) = search_rect {
            place_picker_search_cursor(frame, sr, query, cursor_position);
        }
        return area;
    }

    let body = provider_list_body(
        providers,
        current_provider,
        modal_index,
        theme,
        body_rect.width as usize,
    );
    let follow = if follow_selection {
        Some(modal_index)
    } else {
        None
    };
    render_body(
        frame,
        body_rect,
        body,
        scroll,
        BodyRenderOptions::new(follow, SCROLL_EDGE_MARGIN, false),
        theme,
    );

    if let Some(fo) = f.footer {
        render_modal_footer_with_more(frame, fo, hints, extra, theme);
    }

    if search && let Some(sr) = search_rect {
        place_picker_search_cursor(frame, sr, query, cursor_position);
    }
    area
}

/// Build the **Connections** provider list body via the shared [`crate::components::row::ListRow`]
/// standard.
pub(crate) fn provider_list_body(
    providers: &[RankedProvider],
    _current_provider: &str,
    modal_index: usize,
    theme: &Theme,
    body_width: usize,
) -> Vec<Line<'static>> {
    let name_budget = (body_width / 2).saturating_sub(GUTTER + 1).max(1);

    let mut body: Vec<Line<'static>> = Vec::new();
    for (sel, rp) in providers.iter().enumerate() {
        let is_selected = sel == modal_index;
        let style = choice_style(ChoiceTone::Filled, is_selected, theme);
        let matched = match_set(rp.m.as_ref());

        let name = truncate_ellipsis(&rp.label, name_budget);
        let mut identity = RowGroup::fixed();
        for (char_idx, c) in name.chars().enumerate() {
            let cs = if matched.contains(&char_idx) {
                Style::default()
                    .bg(style.bg)
                    .fg(if is_selected { style.fg } else { theme.brand() })
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
                    .bg(style.bg)
                    .fg(style.fg)
                    .add_modifier(Modifier::BOLD)
            };
            identity = identity.styled(
                RowStyledAtom {
                    text: c.to_string(),
                    style: cs,
                },
                0,
            );
        }

        let mut row = ListRow::new(style, body_width).group(identity);

        if let Some(label) = crate::providers::provider_type_label(&rp.preset_id) {
            row = row.group(RowGroup::midpoint().text(label, style.dim, 0));
        }

        body.push(row.finish());
    }
    body
}

/// The Connections empty-state body: shown when no provider instance exists.
pub(crate) fn connections_empty_body(theme: &Theme) -> Vec<Line<'static>> {
    vec![
        Line::from(Span::styled(
            "No connections yet",
            Style::default().fg(theme.fg()).add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(vec![
            Span::styled("Press ", Style::default().fg(theme.muted())),
            Span::styled(
                "a",
                Style::default()
                    .fg(theme.info())
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" for a preset or ", Style::default().fg(theme.muted())),
            Span::styled(
                "c",
                Style::default()
                    .fg(theme.info())
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" for custom", Style::default().fg(theme.muted())),
        ]),
    ]
}

/// Format a reset time into human-friendly countdown / clock (e.g. "resets in 2h 15m (14:30)").
pub(crate) fn format_reset_countdown(
    reset_at_ms: Option<u64>,
    reset_time_str: Option<&str>,
) -> Option<String> {
    if let Some(reset_ms) = reset_at_ms {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        if reset_ms > now_ms {
            let diff_secs = (reset_ms - now_ms) / 1000;
            let hours = diff_secs / 3600;
            let mins = (diff_secs % 3600) / 60;
            let secs = diff_secs % 60;
            let time_str = if hours > 0 {
                format!("{hours}h {mins}m")
            } else if mins > 0 {
                format!("{mins}m {secs}s")
            } else {
                format!("{secs}s")
            };
            use chrono::{Local, TimeZone};
            let clock = Local
                .timestamp_millis_opt(reset_ms as i64)
                .single()
                .map(|dt| dt.format("%H:%M").to_string())
                .unwrap_or_default();
            if clock.is_empty() {
                return Some(format!("resets in {time_str}"));
            } else {
                return Some(format!("resets in {time_str} ({clock})"));
            }
        } else {
            return Some("resets soon".to_string());
        }
    }
    if let Some(raw) = reset_time_str
        && !raw.trim().is_empty()
    {
        return Some(format!("resets: {raw}"));
    }
    None
}

/// Render a terminal progress bar (e.g. `[████████░░░░░░]`).
pub(crate) fn render_progress_bar_spans(
    used_fraction: f32,
    bar_width: usize,
    theme: &Theme,
) -> Vec<Span<'static>> {
    let clamped = used_fraction.clamp(0.0, 1.0);
    let filled_count = (clamped * bar_width as f32).round() as usize;
    let empty_count = bar_width.saturating_sub(filled_count);

    let color = if clamped >= 0.90 {
        theme.err()
    } else if clamped >= 0.70 {
        theme.warn()
    } else {
        theme.ok()
    };

    let filled_str = "█".repeat(filled_count);
    let empty_str = "░".repeat(empty_count);

    vec![
        Span::styled("[", Style::default().fg(theme.dim())),
        Span::styled(filled_str, Style::default().fg(color)),
        Span::styled(empty_str, Style::default().fg(theme.dim())),
        Span::styled("]", Style::default().fg(theme.dim())),
    ]
}

/// Render the detail body lines for one connection (configuration + caller identity + models + provider usage).
pub(crate) fn connection_detail_body(
    detail: &muta_contracts::ConnectionDetail,
    spinner_phase: usize,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let label = Style::default().fg(theme.dim());
    let value = Style::default().fg(theme.fg());
    let header_style = Style::default()
        .fg(theme.primary)
        .add_modifier(Modifier::BOLD);
    let muted = Style::default().fg(theme.muted());
    let highlight = Style::default().fg(theme.primary);
    let warning = Style::default().fg(theme.warning);

    let kv = |k: &str, v: &str| {
        Line::from(vec![
            Span::styled(format!("{k:<16}"), label),
            Span::styled(v.to_string(), value),
        ])
    };

    let mut lines: Vec<Line<'static>> = Vec::new();

    // ── Configuration ──────────────────────────────────────────────────────────
    lines.push(Line::from(Span::styled("Configuration", header_style)));
    lines.push(kv("ID", &detail.id));
    lines.push(kv("Name", &detail.name));
    if let Some(preset) = &detail.preset_label {
        lines.push(kv("Preset", preset));
    } else if let Some(pid) = &detail.preset_id {
        lines.push(kv("Preset ID", pid));
    } else {
        lines.push(kv("Type", "Custom Connection"));
    }
    lines.push(kv("Protocol", &detail.protocol));
    lines.push(kv("Base URL", &detail.base_url));
    lines.push(kv("Auth Type", &detail.auth_type));
    if let Some(masked) = &detail.api_key_masked {
        lines.push(Line::from(vec![
            Span::styled(format!("{:<16}", "API Key"), label),
            Span::styled(masked.clone(), value),
            Span::styled(format!(" ({})", detail.api_key_source), muted),
        ]));
    } else {
        lines.push(kv("Credential", &detail.api_key_source));
    }
    if let Some(active) = &detail.active_model {
        let mut default_str = active.clone();
        if let Some(effort) = &detail.active_model_effort {
            default_str.push_str(&format!("  ·  reasoning: {effort}"));
        } else if detail.active_model_thinking == Some(true) {
            default_str.push_str("  ·  thinking: enabled");
        }
        lines.push(kv("Default Active", &default_str));
    }

    // ── Client Profile ────────────────────────────────────────────────────────
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled("Client Profile", header_style)));
    lines.push(kv("Preset", detail.client_identity.label()));
    lines.push(kv("User-Agent", &detail.user_agent));
    let client_headers = detail.client_identity.headers();
    if !client_headers.is_empty() {
        lines.push(Line::from(Span::styled(
            format!("{:<16}", "Client Headers"),
            label,
        )));
        for (k, v) in client_headers {
            lines.push(Line::from(vec![
                Span::styled("  • ", label),
                Span::styled(format!("{k}: "), label),
                Span::styled(v.to_string(), value),
            ]));
        }
    }

    // ── Served Models ──────────────────────────────────────────────────────────
    lines.push(Line::from(""));
    let models_title = if detail.models.is_empty() {
        "Served Models".to_string()
    } else {
        format!("Served Models ({})", detail.models.len())
    };
    lines.push(Line::from(Span::styled(models_title, header_style)));
    if detail.models.is_empty() {
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled("(no models configured)", muted),
        ]));
    } else {
        for model in &detail.models {
            let is_active = detail.active_model.as_deref() == Some(model.as_str());
            let dot = if is_active { "  ● " } else { "  ○ " };
            let dot_style = if is_active {
                Style::default().fg(theme.primary)
            } else {
                Style::default().fg(theme.dim())
            };
            let model_style = if is_active {
                value.add_modifier(Modifier::BOLD)
            } else {
                value
            };
            let mut spans = vec![
                Span::styled(dot, dot_style),
                Span::styled(model.clone(), model_style),
            ];
            if let Some(info) = detail.model_info.iter().find(|m| &m.model == model) {
                if let Some(effort) = &info.effort {
                    let show = match info.protocol.as_str() {
                        "anthropic" => info.thinking == Some(true),
                        _ => true,
                    };
                    if show {
                        spans.push(Span::styled(format!("  ·  reasoning: {effort}"), muted));
                    }
                } else if info.thinking == Some(true) {
                    spans.push(Span::styled("  ·  thinking: enabled", muted));
                }
            }
            lines.push(Line::from(spans));
        }
    }

    // ── Provider Usage & Quota ─────────────────────────────────────────────────
    lines.push(Line::from(""));
    let mut quota_header_spans = vec![Span::styled("Provider Usage & Quota", header_style)];
    if let muta_contracts::ConnectionUsageState::Available(usage) = &detail.usage
        && let Some(plan) = &usage.plan
        && plan.len() <= 40
        && !plan.contains('\n')
    {
        quota_header_spans.push(Span::raw("  "));
        quota_header_spans.push(Span::styled(
            format!("[ {plan} ]"),
            Style::default()
                .fg(theme.info())
                .add_modifier(Modifier::BOLD),
        ));
    }
    lines.push(Line::from(quota_header_spans));

    match &detail.usage {
        muta_contracts::ConnectionUsageState::Available(usage) => {
            let mut rendered_quota = false;

            if let Some(quota_data) = &usage.quota {
                match quota_data {
                    muta_contracts::ProviderQuotaData::Periodic(periodic) => {
                        rendered_quota = true;
                        render_periodic_quota_buckets(
                            &periodic.buckets,
                            &mut lines,
                            value,
                            label,
                            muted,
                            highlight,
                            theme,
                        );
                    }
                    muta_contracts::ProviderQuotaData::Balance(balance) => {
                        rendered_quota = true;
                        render_balance_quota_block(
                            balance, &mut lines, label, value, highlight, theme,
                        );
                    }
                    muta_contracts::ProviderQuotaData::Composite {
                        balance,
                        periodic,
                        rate_limits,
                    } => {
                        rendered_quota = true;
                        if let Some(bal) = balance {
                            render_balance_quota_block(
                                bal, &mut lines, label, value, highlight, theme,
                            );
                        }
                        if let Some(per) = periodic {
                            render_periodic_quota_buckets(
                                &per.buckets,
                                &mut lines,
                                value,
                                label,
                                muted,
                                highlight,
                                theme,
                            );
                        }
                        for rl in rate_limits {
                            lines.push(Line::from(vec![
                                Span::raw("  "),
                                Span::styled(format!("{:<16}", "Rate Limit"), label),
                                Span::styled(
                                    format!("{} req / {}", rl.requests, rl.interval),
                                    value,
                                ),
                            ]));
                        }
                    }
                }
            }

            if !rendered_quota {
                if let Some(bal) = &usage.primary_balance {
                    lines.push(Line::from(vec![
                        Span::raw("  "),
                        Span::styled(format!("{:<16}", "Primary Balance"), label),
                        Span::styled(bal.clone(), highlight.add_modifier(Modifier::BOLD)),
                    ]));
                }
                for metric in &usage.metrics {
                    let val = match &metric.unit {
                        Some(u) => format!("{} {}", metric.value, u),
                        None => metric.value.clone(),
                    };
                    lines.push(Line::from(vec![
                        Span::raw("  "),
                        Span::styled(format!("{:<16}", metric.label), label),
                        Span::styled(val, value),
                    ]));
                }
            }

            if let Some(desc) = &usage.description {
                lines.push(Line::from(""));
                let wrapped = crate::text_layout::wrap_text(desc, 72);
                for line in wrapped {
                    lines.push(Line::from(vec![
                        Span::raw("  "),
                        Span::styled(line.text, muted),
                    ]));
                }
            }
        }
        muta_contracts::ConnectionUsageState::Unsupported => {
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    "Usage and quota query is not supported for this provider endpoint.",
                    muted,
                ),
            ]));
        }
        muta_contracts::ConnectionUsageState::Error(err) => {
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled("⚠ Usage query failed: ", warning),
                Span::styled(err.clone(), value),
            ]));
        }
        muta_contracts::ConnectionUsageState::Fetching => {
            const SPINNER_FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
            let spin = SPINNER_FRAMES[spinner_phase % SPINNER_FRAMES.len()];
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(format!("{spin} "), Style::default().fg(theme.primary)),
                Span::styled("Querying upstream provider quota & balance…", muted),
            ]));
        }
    }

    lines
}

pub(crate) fn render_balance_quota_block(
    balance: &muta_contracts::BalanceQuota,
    lines: &mut Vec<Line<'static>>,
    label: Style,
    value: Style,
    highlight: Style,
    theme: &Theme,
) {
    if let (Some(consumed), Some(limit)) = (balance.consumed_amount, balance.credit_limit)
        && limit > 0.0
    {
        let frac = (consumed / limit) as f32;
        let pct = (frac * 100.0).round() as u32;
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(
                "Credit Limit Consumption",
                value.add_modifier(Modifier::BOLD),
            ),
        ]));
        let mut bar_spans = vec![Span::raw("    ")];
        bar_spans.extend(render_progress_bar_spans(frac, 20, theme));
        bar_spans.push(Span::styled(
            format!("  ${:.2} / ${:.2} ({pct}% used)", consumed, limit),
            value,
        ));
        lines.push(Line::from(bar_spans));
    }

    if let Some(total) = balance.total_balance {
        let sym = if balance.currency == "CNY" {
            "¥"
        } else if balance.currency == "USD" {
            "$"
        } else {
            ""
        };
        let total_str = if !sym.is_empty() {
            format!("{sym}{:.2} {}", total, balance.currency)
        } else {
            format!("{:.2} {}", total, balance.currency)
        };
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(format!("{:<16}", "Total Balance"), label),
            Span::styled(total_str, highlight.add_modifier(Modifier::BOLD)),
        ]));
    } else if let Some(prim) = &balance.display_primary {
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(format!("{:<16}", "Balance"), label),
            Span::styled(prim.clone(), highlight.add_modifier(Modifier::BOLD)),
        ]));
    }

    if let Some(cash) = balance.cash_balance {
        let sym = if balance.currency == "CNY" {
            "¥"
        } else if balance.currency == "USD" {
            "$"
        } else {
            ""
        };
        lines.push(Line::from(vec![
            Span::raw("    ├─ Recharge:     "),
            Span::styled(format!("{sym}{:.2}", cash), value),
        ]));
    }
    if let Some(voucher) = balance.voucher_balance {
        let sym = if balance.currency == "CNY" {
            "¥"
        } else if balance.currency == "USD" {
            "$"
        } else {
            ""
        };
        lines.push(Line::from(vec![
            Span::raw("    └─ Voucher:      "),
            Span::styled(format!("{sym}{:.2}", voucher), value),
        ]));
    }
}

pub(crate) fn render_periodic_quota_buckets(
    buckets: &[muta_contracts::QuotaWindowBucket],
    lines: &mut Vec<Line<'static>>,
    value: Style,
    _label: Style,
    muted: Style,
    highlight: Style,
    theme: &Theme,
) {
    if buckets.is_empty() {
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled("Active (no specific bucket limits reported)", muted),
        ]));
        return;
    }

    let mut last_group: Option<&str> = None;
    for bucket in buckets {
        let current_group = bucket.group.as_deref();
        if current_group != last_group {
            if let Some(grp) = current_group {
                if last_group.is_some() {
                    lines.push(Line::from(""));
                }
                lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(format!("▸ {grp}"), highlight.add_modifier(Modifier::BOLD)),
                ]));
            }
            last_group = current_group;
        }

        let has_group = bucket.group.is_some();
        let title_indent = if has_group { "    " } else { "  " };
        let bar_indent = if has_group { "      " } else { "    " };

        let window_tag = bucket
            .window
            .map(|w| format!(" · {}", w.label()))
            .unwrap_or_default();
        lines.push(Line::from(vec![
            Span::raw(title_indent),
            Span::styled(
                format!("{}{}", bucket.label, window_tag),
                value.add_modifier(Modifier::BOLD),
            ),
        ]));

        let pct_used = (bucket.used_fraction * 100.0).round() as u32;
        let pct_rem = 100u32.saturating_sub(pct_used);
        let mut bar_spans = vec![Span::raw(bar_indent)];
        bar_spans.extend(render_progress_bar_spans(bucket.used_fraction, 20, theme));
        bar_spans.push(Span::styled(
            format!("  {pct_used}% used ({pct_rem}% remaining)"),
            value,
        ));
        if let Some(reset_str) =
            format_reset_countdown(bucket.reset_at_ms, bucket.reset_time_str.as_deref())
        {
            bar_spans.push(Span::styled(format!("  ·  {reset_str}"), muted));
        }
        lines.push(Line::from(bar_spans));
    }
}
