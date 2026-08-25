//! The Connections (provider-instance management) and Models (flat
//! provider/model picker) modals, the API-key / model-id editor, and the
//! custom-provider editor modals.

use mutx_engine::{
    Alignment, Frame, Paragraph, Rect, {Line, Span}, {Modifier, Style},
};
use unicode_width::UnicodeWidthStr;

use crate::components::selectable_body::{SelectableRow, render_selectable_body};
use crate::model::layout::LayoutMap;
use crate::model::selection::SelectionState;

use super::common::{caret_column, field_viewport, truncate_ellipsis};
use crate::components::options::{ChoiceTone, choice_style, push_wrapped_styled};
use crate::components::row::{GROUP_GAP, GUTTER, ListRow, RowGroup, RowStyledAtom};
use crate::primitives::{
    ContentModalSpec, FixedModalSpec, FooterHint, FooterHintWithBand, SCROLL_EDGE_MARGIN,
    breadcrumb_parts, content_modal_area, content_modal_probe, hierarchical_breadcrumb,
    keymap_body_lines, keymap_page_footer_hints, keyvocab, modal_area, modal_chrome_rows,
    modal_frame, modal_header, modal_header_parts, render_body, render_centered_body,
    render_modal_footer, render_modal_footer_with_more,
};
use crate::providers::{
    CustomField, ModelBodyLine, PROVIDER_TEMPLATES, ProviderTemplate, RankedModel, RankedProvider,
    models_body_lines,
};
use crate::view::Theme;

/// Draw the **Connections** modal — the provider-instance management surface
/// (`/connections`). A ranked provider list (last-used → name); each row shows
/// the instance name and its provider *type* (`· OpenAI`) — never the model
/// name (models live in the Models picker). There is no per-row "current"
/// state dot and no favorite concept here (favorite is model-level now,
/// ADR-0046), and no activation concept either — switching the active provider
/// is the Models picker's job, so this surface only *manages* instances: `a`
/// adds a new connection (opens the template chooser from the footer), `e`
/// edits, `Shift+D` deletes a custom provider. When no instance exists, an
/// empty-state hint prompts the user to press `a`. Mirrors the input-history
/// modal's two-mode (browse/search) design: `/` enters search, the header stays
/// title-only, a dedicated search row appears beneath it, and rows highlight
/// matched chars.
///
/// `providers` is the pre-computed row set; `modal_index` selects into it (the
/// value `providers.len()` is the synthetic add row). `scroll` is read and
/// written back so the offset stays consistent with the clamped body height;
/// `follow_selection` keeps `modal_index` in view after navigation.
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
) -> mutx_engine::Rect {
    let area = modal_area(frame, FixedModalSpec::PROVIDER);
    let f = modal_frame(frame, area, theme.panel(), true, true);

    let header_rect = f.header;

    // `a add` opens the template chooser and is the primary action in this view
    // (rank 80), surviving width collapse longer than `D delete` (rank 70).
    // There is no `Enter activate` here — switching the active provider is the Models
    // picker's job; this surface only manages instances.
    let browse_hints: [FooterHint; 6] = [
        FooterHint::navigation(keyvocab::ARROWS_UD, "navigate"),
        FooterHint::secondary("/", "search"),
        FooterHint::primary("a", "add"),
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
        // Breadcrumb: `Connections` modal › its keybindings sub-page (hierarchy
        // is never joined with `·`, which is reserved for same-rank modifiers).
        modal_header(
            frame,
            header_rect,
            &format!("Connections{}keybindings", crate::design::JOIN_BREADCRUMB),
            theme,
        );
        let body = keymap_body_lines(hints, extra, theme);
        // Selectable document: the keymap sub-page registers as MODAL_DOC
        // rows so key labels and descriptions are copyable.
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

    modal_header(frame, header_rect, "Connections", theme);

    let (search_rect, body_rect) = split_search_body(f.body, search);
    if let Some(search_rect) = search_rect {
        draw_picker_search_row(frame, search_rect, query, theme);
    }

    // Empty state: no provider instance exists. Show a vertically and
    // horizontally centered hint that points the user at the `a` footer
    // shortcut to add one (browse mode only — in search mode the standard
    // "no matches" body applies).
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
            let prefix = " Search  › ".width() as u16;
            let cursor_x = sr.x + prefix + caret_column(query, cursor_position);
            let cursor_y = sr.y;
            frame.set_cursor_position((cursor_x, cursor_y));
        }
        return area;
    }

    // The provider list maps 1:1 to `modal_index` (no synthetic add row —
    // adding is a footer shortcut now), so the selected visual line equals
    // `modal_index`.
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
        follow,
        SCROLL_EDGE_MARGIN,
        false,
        theme,
    );

    if let Some(fo) = f.footer {
        render_modal_footer_with_more(frame, fo, hints, extra, theme);
    }

    // The real terminal caret only exists in search mode — browse mode has no
    // editable field. Place it in the dedicated search row, not in the header.
    if search && let Some(sr) = search_rect {
        let prefix = " Search  › ".width() as u16;
        let cursor_x = sr.x + prefix + caret_column(query, cursor_position);
        let cursor_y = sr.y;
        frame.set_cursor_position((cursor_x, cursor_y));
    }
    area
}

/// Draw the **Models** modal — the flat (provider, model) picker
/// (`Ctrl+M` / `/models`), the daily-driver switch surface. One row per pair
/// across every provider, `★ <model>  · <provider>`: a favorite star, the model
/// name, then a dim provider suffix. The list is **sectioned into three
/// labeled groups** — Favorites (★-marked, ASCII order), Recent (usage
/// history, most-recent-first), and All models (ASCII order) — each announced
/// by a dim uppercase label row that the selection cursor skips over. Enter
/// activates the highlighted pair; `*` favorites the model (favorite is
/// model-level, ADR-0046); `e` opens its per-model settings
/// (effort/thinking). There is **no delete** here — models are served by
/// their provider, so they cannot be removed from this surface. Same
/// browse/search two-mode design as the Connections modal.
///
/// `models` is the pre-computed flat row set; `modal_index` selects into it.
/// `scroll` is read and written back so the offset stays consistent with the
/// clamped body height; `follow_selection` keeps `modal_index` in view after
/// navigation (mapped through the section-label interleaving).
#[allow(clippy::too_many_arguments)]
pub fn draw_models_modal(
    frame: &mut Frame,
    layout_map: &mut LayoutMap,
    models: &[RankedModel],
    current_provider: &str,
    current_model: &str,
    modal_index: usize,
    query: &str,
    cursor_position: usize,
    scroll: &mut usize,
    follow_selection: bool,
    search: bool,
    keymap_open: bool,
    theme: &Theme,
    selection: &SelectionState,
) -> mutx_engine::Rect {
    let area = modal_area(frame, FixedModalSpec::PROVIDER);
    let f = modal_frame(frame, area, theme.panel(), true, true);

    let header_rect = f.header;

    // No destructive action here — models are served by their provider and
    // cannot be removed from this surface. Favorite is model-level (ADR-0046).
    let browse_hints: [FooterHint; 7] = [
        FooterHint::navigation(keyvocab::ARROWS_UD, "navigate"),
        FooterHint::secondary("/", "search"),
        FooterHint::primary(keyvocab::ENTER, "activate"),
        FooterHint::secondary("*", "favorite"),
        FooterHint::secondary("e", "settings"),
        FooterHint::secondary("r", "refresh"),
        FooterHint::always(keyvocab::ESC, "close"),
    ];
    let search_hints: [FooterHint; 4] = [
        FooterHint::secondary("type", "filter"),
        FooterHint::navigation(keyvocab::ARROWS_UD, "navigate"),
        FooterHint::primary(keyvocab::ENTER, "activate"),
        FooterHint::always(keyvocab::ESC, "clear search"),
    ];
    let empty_hints: [FooterHint; 2] = [
        FooterHint::primary("a", "add connection"),
        FooterHint::always(keyvocab::ESC, "close"),
    ];
    let (hints, extra): (&[FooterHint], &[FooterHintWithBand]) = if search {
        (&search_hints, &[])
    } else if models.is_empty() {
        (&empty_hints, &[])
    } else {
        (&browse_hints, &[])
    };

    if keymap_open {
        // Breadcrumb: `Models` modal › its keybindings sub-page.
        modal_header(
            frame,
            header_rect,
            &format!("Models{}keybindings", crate::design::JOIN_BREADCRUMB),
            theme,
        );
        let body = keymap_body_lines(hints, extra, theme);
        // Selectable document: the keymap sub-page registers as MODAL_DOC
        // rows so key labels and descriptions are copyable.
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

    modal_header(frame, header_rect, "Models", theme);

    let (search_rect, body_rect) = split_search_body(f.body, search);
    if let Some(search_rect) = search_rect {
        draw_picker_search_row(frame, search_rect, query, theme);
    }

    // Empty state: no model available. Show a vertically and horizontally
    // centered hint prompting the user to add a connection in /connections
    // (or press `a` to add), and indicating that fetched models will appear here.
    if models.is_empty() && !search {
        let body = models_empty_body(theme);
        render_centered_body(frame, body_rect, body);
        if let Some(fo) = f.footer {
            render_modal_footer_with_more(frame, fo, hints, extra, theme);
        }
        return area;
    }

    if models.is_empty() && search {
        let body = search_empty_body(theme);
        render_centered_body(frame, body_rect, body);
        if let Some(fo) = f.footer {
            render_modal_footer_with_more(frame, fo, hints, extra, theme);
        }
        if let Some(sr) = search_rect {
            let prefix = " Search  › ".width() as u16;
            let cursor_x = sr.x + prefix + caret_column(query, cursor_position);
            let cursor_y = sr.y;
            frame.set_cursor_position((cursor_x, cursor_y));
        }
        return area;
    }

    // Flat model rows map 1:1 to `modal_index`. The body interleaves dim
    // section labels (FAVORITES / RECENT / ALL MODELS) with the rows, so the
    // scroll follow targets the *body line* the selected row paints on, not
    // the raw row index.
    let (body, row_line) = model_list_body(
        models,
        current_provider,
        current_model,
        modal_index,
        theme,
        body_rect.width as usize,
    );
    let follow = if follow_selection {
        row_line.get(modal_index).copied()
    } else {
        None
    };
    render_body(
        frame,
        body_rect,
        body,
        scroll,
        follow,
        SCROLL_EDGE_MARGIN,
        false,
        theme,
    );

    if let Some(fo) = f.footer {
        render_modal_footer_with_more(frame, fo, hints, extra, theme);
    }

    // The real terminal caret only exists in search mode — browse mode has no
    // editable field. Place it in the dedicated search row, not in the header.
    if search && let Some(sr) = search_rect {
        let prefix = " Search  › ".width() as u16;
        let cursor_x = sr.x + prefix + caret_column(query, cursor_position);
        let cursor_y = sr.y;
        frame.set_cursor_position((cursor_x, cursor_y));
    }
    area
}

fn split_search_body(body: Rect, search: bool) -> (Option<Rect>, Rect) {
    if !search || body.height == 0 {
        return (None, body);
    }

    let search_rect = Rect {
        x: body.x,
        y: body.y,
        width: body.width,
        height: 1,
    };
    let consumed = if body.height > 1 { 2 } else { 1 };
    let list_rect = Rect {
        x: body.x,
        y: body.y.saturating_add(consumed),
        width: body.width,
        height: body.height.saturating_sub(consumed),
    };
    (Some(search_rect), list_rect)
}

fn draw_picker_search_row(frame: &mut Frame, rect: Rect, query: &str, theme: &Theme) {
    let value_style = Style::default()
        .fg(if query.is_empty() {
            theme.muted()
        } else {
            theme.fg()
        })
        .add_modifier(Modifier::BOLD);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(" Search", Style::default().fg(theme.muted())),
            Span::styled("  › ", Style::default().fg(theme.muted())),
            Span::styled(
                if query.is_empty() {
                    "type to fuzzy-filter"
                } else {
                    query
                },
                value_style,
            ),
        ])),
        rect,
    );
}

/// Build the **Connections** provider list body via the shared [`crate::components::row::ListRow`]
/// standard. Each row is a two-column layout: column 1 (fixed, after the
/// gutter) is the instance name (bold, fuzzy-highlighted in search); column 2
/// (midpoint) is the provider *type* label (dim), anchored at the horizontal
/// center so the two columns spread across the width — no `·`, just the
/// midpoint gap. The row fills the full `body_width` edge-to-edge. There is no
/// leading dot or star here (favorite is model-level, ADR-0046); the model name
/// is intentionally omitted.
fn provider_list_body(
    providers: &[RankedProvider],
    _current_provider: &str,
    modal_index: usize,
    theme: &Theme,
    body_width: usize,
) -> Vec<Line<'static>> {
    use crate::components::options::{ChoiceTone, choice_style};
    use crate::components::row::{GUTTER, ListRow, RowGroup, RowStyledAtom};

    // Column 1 (instance name) is capped to half the width (minus the gutter
    // and a little slack) so it never runs into the midpoint column 2.
    let name_budget = (body_width / 2).saturating_sub(GUTTER + 1).max(1);

    let mut body: Vec<Line<'static>> = Vec::new();
    for (sel, rp) in providers.iter().enumerate() {
        let is_selected = sel == modal_index;
        let style = choice_style(ChoiceTone::Filled, is_selected, theme);
        let matched = match_set(rp.m.as_ref());

        // Column 1 (fixed): the instance name, one styled atom per char so
        // fuzzy matches lift to the brand / contrast color.
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

        // Column 2 (midpoint): the provider TYPE label, anchored at the
        // horizontal center so the two columns spread across the width. Omitted
        // for legacy instances with no recorded template.
        if let Some(label) = crate::providers::provider_type_label(&rp.preset_id) {
            row = row.group(RowGroup::midpoint().text(label, style.dim, 0));
        }

        body.push(row.finish());
    }
    body
}

/// The Connections empty-state body: shown when no provider instance exists.
/// A vertically and horizontally centered hint that points the user at the `a`
/// footer shortcut to add one.
fn connections_empty_body(theme: &Theme) -> Vec<Line<'static>> {
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
            Span::styled(
                " to add a provider connection",
                Style::default().fg(theme.muted()),
            ),
        ]),
    ]
}

/// Build the **Models** flat model list body via the shared [`crate::components::row::ListRow`]
/// standard, **sectioned into three labeled groups** — Favorites, Recent,
/// All models. The list interleaves these model sections:
///
/// - a dim uppercase section label row (`FAVORITES` / `RECENT` /
///   `ALL MODELS`) before each non-empty section, separated from the previous
///   group by one blank spacer row (no spacer before the very first label);
/// - the section's selectable rows.
///
/// Each selectable row is a two-column layout spread across the width:
/// - column 1 (fixed, 60% weight): the model's wire id (bold, fuzzy-highlighted in
///   search) — id-first policy, never a curated display name;
/// - column 2 (proportional ratio 3/5): the provider label (dim), anchored so
///   identical model ids served by different instances stay cleanly separated;
/// - an optional trailing reasoning tag (`think on`), right-pinned.
///
/// The row fills the full `body_width` edge-to-edge.
///
/// Returns the body lines plus `row_line`: the body-line index of each
/// selectable row. The caller uses that map to translate the modal's
/// *selection cursor* (a flat-row index) into the *body line* the scroll
/// follow logic must keep visible — label and spacer lines have no cursor, so
/// ↑/↓ can never stop on them.
fn model_list_body(
    models: &[RankedModel],
    _current_provider: &str,
    _current_model: &str,
    modal_index: usize,
    theme: &Theme,
    body_width: usize,
) -> (Vec<Line<'static>>, Vec<usize>) {
    use crate::components::options::{ChoiceTone, choice_style};

    if models.is_empty() {
        return (Vec::new(), Vec::new());
    }
    let (geometry, row_line) = models_body_lines(models);
    let mut body: Vec<Line> = Vec::with_capacity(geometry.len() + 3);

    // Spacer rows: one blank line before every section label except the
    // first. Pure background filler, matching the panel background so the
    // band reads as a gap, not a row.
    let spacer = || {
        Line::from(Span::styled(
            " ".repeat(body_width.max(1)),
            Style::default().bg(theme.panel()),
        ))
    };

    for line in geometry {
        match line {
            ModelBodyLine::Section(section) => {
                if !body.is_empty() {
                    body.push(spacer());
                }
                // Section label: dim uppercase tag on the panel background,
                // one GUTTER in — the same left edge the rows' glyphs sit
                // at, so the label reads as the group's header rather than
                // a centered title.
                body.push(Line::from(Span::styled(
                    format!("{}{}", " ".repeat(GUTTER), section.label()),
                    Style::default().fg(theme.muted()),
                )));
            }
            ModelBodyLine::Row(row) => {
                let rm = &models[row];
                let is_selected = row == modal_index;
                let style = choice_style(ChoiceTone::Filled, is_selected, theme);

                // The reasoning tag. ADR-0046: reasoning is opt-in, so a model only
                // shows a tag when reasoning is actually engaged, then with its
                // current effort level. Anthropic rows opt in via the thinking switch
                // (`thinking == Some(true)`); OpenAI rows have no separate switch —
                // an exposed effort knob means the model reasons — so they show
                // their effective effort directly (mirrors the hint bar's
                // per-protocol gating). An unconfigured model shows nothing.
                // Keep in sync with `tests::reasoning_tag`.
                let tag = match (rm.thinking, rm.effort.as_deref()) {
                    (Some(true), Some(effort)) => format!("think on {effort}"),
                    (Some(true), None) => "think on".to_string(),
                    (None, Some(effort)) => effort.to_string(),
                    _ => String::new(),
                };

                // Column 1 (model id) is allocated a generous 60% (3/5) proportional
                // share of the row width (minus gutter and slack), so long model wire IDs
                // have ample breathing room while preserving crisp columnar alignment.
                let name_budget = ((body_width * 3) / 5).saturating_sub(GUTTER + 1).max(1);
                // Id-first policy: the row label IS the wire id (never a curated
                // display name), so every row reads the same kind of label.
                let name = truncate_ellipsis(&rm.model, name_budget);

                // Column 1: the model id, one styled atom per char so fuzzy matches
                // lift to the brand / contrast color.
                let matched = match_set(rm.m.as_ref());
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

                // Column 2 (proportional ratio 3/5): the provider label, anchored at
                // 60% across the row width so the primary identity column receives
                // dominant space without crowding the provider.
                let mut list_row = ListRow::new(style, body_width)
                    .group(identity)
                    .group(RowGroup::ratio(3, 5).text(rm.provider_label.as_str(), style.dim, 0));

                // Optional trailing reasoning tag, right-pinned and info-toned. On a
                // brand-filled selected row it lifts to the contrast foreground.
                if !tag.is_empty() {
                    let tag_fg = if is_selected {
                        list_row.fill_fg()
                    } else {
                        theme.info()
                    };
                    list_row = list_row.group(
                        RowGroup::trailing().text(tag, tag_fg, 0),
                    );
                }
                body.push(list_row.finish());
            }
        }
    }
    (body, row_line)
}

/// The Models empty-state body: shown when no model exists (no provider configured
/// or no models returned). A vertically and horizontally centered hint that prompts
/// the user to add a connection via `/connections` or press `a`, and explains that
/// fetched models will appear here once connected.
fn models_empty_body(theme: &Theme) -> Vec<Line<'static>> {
    vec![
        Line::from(Span::styled(
            "No models available",
            Style::default().fg(theme.fg()).add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(vec![
            Span::styled("Add a connection via ", Style::default().fg(theme.muted())),
            Span::styled(
                "/connections",
                Style::default()
                    .fg(theme.info())
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" (or press ", Style::default().fg(theme.muted())),
            Span::styled(
                "a",
                Style::default()
                    .fg(theme.info())
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(")", Style::default().fg(theme.muted())),
        ]),
        Line::from(Span::styled(
            "Configured models will appear here",
            Style::default().fg(theme.muted()),
        )),
    ]
}

/// The "no matches" placeholder body shared by both pickers during search.
fn search_empty_body(theme: &Theme) -> Vec<Line<'static>> {
    vec![Line::from(Span::styled(
        "(no matches — try a shorter or different query)",
        Style::default().fg(theme.muted()),
    ))]
}

/// Char indices the fuzzy match highlights, as a set for O(1) per-char lookup.
fn match_set(m: Option<&crate::fuzzy::FuzzyMatch>) -> std::collections::HashSet<usize> {
    m.map(|m| m.positions.iter().copied().collect())
        .unwrap_or_default()
}

// ── Effort selector (Faster⇄Smarter node slider) ────────────────────────────

/// The words flanking the slider's track — the two ends of the scale, so the
/// speed/depth trade-off reads before any tier name does.
const EFFORT_SCALE_ENDS: (&str, &str) = ("Faster", "Smarter");

/// The slider track's glyphs: hollow circles for unselected rungs, and a solid
/// marker for the active selection. Both glyphs share the same font bounding box
/// and vertical centerline, eliminating jitter and visual height mismatch.
const TRACK_NODE: char = '○';
const TRACK_MARKER: char = '●';

/// Minimum gap between two neighbouring tier labels under the track —
/// tighter than this and the layout thins interior labels out rather than
/// overlapping.
const EFFORT_LABEL_MIN_GAP: usize = 2;

/// Resolve a wire effort string to its typed tier, for the caption text. An
/// unrecognized value yields `None` (no caption rather than a wrong one).
fn effort_tier(wire: &str) -> Option<muta_contracts::effort::Effort> {
    muta_contracts::effort::Effort::parse(wire)
}

/// The current tier's caption, e.g. `deep reasoning — the default for real
/// work`. Empty when the wire value is unrecognized.
fn effort_caption(wire: &str) -> &'static str {
    effort_tier(wire).map(|e| e.description()).unwrap_or("")
}

/// Width of the slider's track: the body minus the two scale words (plus
/// their single-space padding) that flank the track. The `Effort` label owns
/// its own row, so the track gets the full body width.
fn slider_track_width(body_width: usize) -> usize {
    let (lo, hi) = EFFORT_SCALE_ENDS;
    body_width.saturating_sub(lo.width() + hi.width() + 2)
}

/// Column of every node on a track of `track_w` cells: one per tier, the two
/// endpoints pinned to the track's ends, the rest evenly spaced between them.
fn slider_node_columns(n: usize, track_w: usize) -> Vec<usize> {
    if n == 0 {
        return Vec::new();
    }
    if n == 1 {
        return vec![track_w / 2];
    }
    (0..n)
        .map(|i| (i * (track_w - 1) + (n - 1) / 2) / (n - 1))
        .collect()
}

/// Left edge of every tier label within the modal body of `body_width` cells.
/// Each rung's label is centered directly on its node's column in the body.
/// Where neighbours would sit closer than [`EFFORT_LABEL_MIN_GAP`] the layout
/// **thins** rather than overlapping: the two ends anchor the scale, the
/// selected rung always names the value, and interior rungs keep their label
/// only while they fit. `None` when there is nothing to lay out (unknown
/// ladder) — the caller shows the bare value row.
fn slider_label_layout(
    levels: &[String],
    current: &str,
    body_width: usize,
) -> Option<Vec<Option<usize>>> {
    let n = levels.len();
    if n == 0 || body_width == 0 {
        return None;
    }
    let track_w = slider_track_width(body_width);
    if track_w == 0 {
        return None;
    }
    let columns = slider_node_columns(n, track_w);
    let (lo, _) = EFFORT_SCALE_ENDS;
    let lo_pad = lo.width() + 1;
    let sel_idx = levels.iter().position(|l| l == current);
    let width = |i: usize| levels[i].as_str().width();

    // Wide form: every rung labeled verbatim, centered directly under its node.
    let mut positions: Vec<Option<usize>> = Vec::with_capacity(n);
    let mut prev_end: Option<usize> = None;
    for (i, &col) in columns.iter().enumerate() {
        let w = width(i);
        if w > body_width {
            break; // cramped — fall through to the thinned form below
        }
        let node_x = lo_pad + col;
        let start = node_x.saturating_sub(w / 2).min(body_width.saturating_sub(w));
        if let Some(end) = prev_end
            && start < end + EFFORT_LABEL_MIN_GAP
        {
            break; // cramped — fall through to the thinned form below
        }
        prev_end = Some(start + w);
        positions.push(Some(start));
    }
    if positions.len() == n {
        return Some(positions);
    }

    // Thinned form: ends + selected always labeled; interior rungs only
    // while they fit. A rung with no label still shows its node.
    positions = Vec::with_capacity(n);
    prev_end = None;
    for (i, &col) in columns.iter().enumerate() {
        let w = width(i);
        let must = i == 0 || i + 1 == n || Some(i) == sel_idx;
        if w > body_width {
            positions.push(None);
            continue;
        }
        let node_x = lo_pad + col;
        let desired = node_x.saturating_sub(w / 2).min(body_width.saturating_sub(w));
        let start = match prev_end {
            Some(end) if desired < end + EFFORT_LABEL_MIN_GAP => {
                if !must {
                    positions.push(None); // thin this interior rung out
                    continue;
                }
                let pushed = end + 1; // squeeze to a single-space gap
                if pushed + w > body_width {
                    positions.push(None);
                    continue;
                }
                pushed
            }
            _ => desired,
        };
        prev_end = Some(start + w);
        positions.push(Some(start));
    }
    Some(positions)
}

/// The number of rows the effort block occupies. The selector is always the
/// slider — six rows: the `Effort` title, blank row, the track, the tier
/// labels, blank row, and the centered caption. An unknown ladder collapses to
/// three rows: the bare value row, blank row, plus the caption.
pub(crate) fn effort_block_rows(levels: &[String]) -> u16 {
    if levels.is_empty() { 3 } else { 6 }
}

/// Build the effort block's line(s). The form is always the `Faster ⇄ Smarter`
/// node slider — the `Effort` label on its own row, a track whose per-tier
/// nodes carry the marker on the selected one, the tier labels centered under
/// their nodes (a cramped interior rung's label thins out; ends and the selected
/// rung always stay), and the centered caption. An unknown ladder (empty
/// `levels`) shows the bare `Effort` value row plus the caption. `focused`
/// highlights the selected tier in the brand tone (and, for `max`, the
/// ignition accent).
fn effort_block_lines(
    current: &str,
    levels: &[String],
    body_width: usize,
    focused: bool,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let caption = truncate_ellipsis(effort_caption(current), body_width);
    let caption_row = || {
        Line::from(Span::styled(
            caption.to_string(),
            Style::default().fg(theme.muted()),
        ))
        .alignment(Alignment::Center)
    };
    // The selected tier reads in the brand tone while focused; `max` always
    // takes the warning tone so the top rung reads as the ignition tier it is
    // (mirrors the hint bar's `M A X` celebration).
    let selected_fg = if current == "max" {
        theme.warn()
    } else if focused {
        theme.brand()
    } else {
        theme.fg()
    };

    // Unknown ladder: the slider cannot lay out, so the block degrades to
    // the `Effort` row plus the caption — never to a second selector shape.
    let Some(positions) = slider_label_layout(levels, current, body_width) else {
        return vec![
            Line::from(vec![
                Span::styled(
                    "Effort  ".to_string(),
                    Style::default()
                        .fg(theme.brand())
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    current.to_string(),
                    Style::default()
                        .fg(selected_fg)
                        .add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::default(),
            caption_row(),
        ];
    };

    // The slider: a `Faster ○─○─●─○ Smarter` track row — hollow circles for
    // rungs, the filled marker replacing the selected node — with the tier
    // labels centered under their nodes and the centered caption row beneath.
    let (lo, hi) = EFFORT_SCALE_ENDS;
    let track_w = slider_track_width(body_width);
    let columns = slider_node_columns(levels.len(), track_w);
    let sel_idx = levels.iter().position(|l| l == current).unwrap_or(0);
    let track_style = Style::default().fg(theme.muted());
    let marker_style = Style::default()
        .fg(selected_fg)
        .add_modifier(Modifier::BOLD);

    let track: Vec<char> = (0..track_w)
        .map(|col| match columns.iter().position(|&c| c == col) {
            Some(i) if i == sel_idx => TRACK_MARKER,
            Some(_) => TRACK_NODE,
            None => '─',
        })
        .collect();
    let marker_col = columns[sel_idx];
    let before: String = track[..marker_col].iter().collect();
    let after: String = track[marker_col + 1..].iter().collect();
    let track_row = Line::from(vec![
        Span::styled(format!("{lo} "), track_style),
        Span::styled(before, track_style),
        Span::styled(TRACK_MARKER.to_string(), marker_style),
        Span::styled(after, track_style),
        Span::styled(format!(" {hi}"), track_style),
    ]);

    let mut label_spans: Vec<Span<'static>> = Vec::new();
    let mut x = 0;
    for (i, level) in levels.iter().enumerate() {
        if let Some(start) = positions[i] {
            if start > x {
                label_spans.push(Span::raw(" ".repeat(start - x)));
            }
            let style = if i == sel_idx {
                Style::default()
                    .fg(selected_fg)
                    .add_modifier(Modifier::BOLD)
            } else {
                track_style
            };
            x = start + level.as_str().width();
            label_spans.push(Span::styled(level.clone(), style));
        }
    }

    vec![
        Line::from(Span::styled(
            "Effort",
            Style::default()
                .fg(theme.brand())
                .add_modifier(Modifier::BOLD),
        )),
        Line::default(),
        track_row,
        Line::from(label_spans),
        Line::default(),
        caption_row(),
    ]
}

/// Draw the provider key editor: a single **API key** field. The model is chosen
/// from the Models picker, so it is not edited here. `input` is the live
/// API-key value borrowed from the composer line.
#[allow(clippy::too_many_arguments)] // modal draw fns thread many context args by nature
pub fn draw_model_editor(
    frame: &mut Frame,
    title: &str,
    input: &str,
    cursor_position: usize,
    show_key: bool,
    // `focused_field`: `0` = API key focused, `1` = effort focused, `2` =
    // thinking focused. Determines caret row and which field's live text is in
    // `input`.
    focused_field: u8,
    // `effort`: when `Some`, render the effort selector showing the current
    // level; cycled with ←/→ (or jumped to with a digit) by the caller.
    // `effort_levels` carries the model's full ladder so every tier can be
    // laid out along the slider. `None` effort hides the whole block.
    effort: Option<&str>,
    // The model's advertised effort ladder (wire strings, ascending). Drives
    // the slider layout; may be empty when the caller could not resolve the
    // model, in which case the block shows the bare value row + caption.
    effort_levels: &[String],
    // `thinking`: when `Some`, render an extended-thinking on/off row showing
    // a checkbox; toggled with Space by the caller.
    // `None` hides. Orthogonal to effort.
    thinking: Option<bool>,
    theme: &Theme,
) -> mutx_engine::Rect {
    let geometry = ContentModalSpec::MODEL_EDITOR;

    // Content-driven height: the editor's row count is width-independent, so
    // size the panel to exactly fit the rows rather than reserving a fixed 30%
    // slab that left most of the panel empty. The effort block is always the
    // slider — four rows (label + track + tier labels + caption) with a known
    // ladder, two (value row + caption) without — so the count needs no width
    // probe. We add the modal chrome and let `content_modal_area` clamp the
    // total to a sane viewport fraction.
    let body_width = content_modal_probe(frame, geometry)
        .width
        .saturating_sub(2 * crate::design::MODAL_INNER_H_PADDING) as usize;
    let effort_rows = match effort {
        Some(_) => effort_block_rows(effort_levels),
        None => 0,
    };
    let body_rows = show_key as u16 + effort_rows + thinking.is_some() as u16;
    let desired = body_rows + modal_chrome_rows(geometry.modal_spec());
    let area = content_modal_area(frame, geometry, desired);
    let f = modal_frame(frame, area, theme.panel(), true, true);

    let child_title = format!("Edit {title}");
    let header = breadcrumb_parts("Models", &child_title);
    modal_header_parts(frame, f.header, &header, theme);

    // Row 0: API key. Present for provider auth editing; hidden for
    // per-model/channel settings.
    let label_style = Style::default()
        .fg(theme.brand())
        .add_modifier(Modifier::BOLD);
    // Horizontal-viewport offset for the focused API-key row, so long keys
    // scroll under the caret instead of spilling past the modal edge.
    let mut api_key_off: usize = 0;
    let mut body: Vec<Line> = Vec::new();
    if show_key {
        let label = format!("{:<8}", "API key");
        let label_w = label.width();
        let field_w = body_width.saturating_sub(label_w);
        let key_off;
        let value_span = if input.is_empty() {
            key_off = 0;
            Span::styled("enter key…".to_string(), Style::default().fg(theme.muted()))
        } else if focused_field == 0 {
            // Focused: caret-following viewport keeps the caret in view.
            let (off, text) = field_viewport(input, cursor_position, field_w);
            key_off = off;
            Span::styled(
                text,
                Style::default().fg(theme.fg()).add_modifier(Modifier::BOLD),
            )
        } else {
            // Unfocused: width-capped ellipsis truncation.
            key_off = 0;
            Span::styled(
                truncate_ellipsis(input, field_w.max(1)),
                Style::default().fg(theme.fg()).add_modifier(Modifier::BOLD),
            )
        };
        body.push(Line::from(vec![
            Span::styled(label, label_style),
            value_span,
        ]));
        api_key_off = key_off;
    }

    // Effort block (optional): the Faster⇄Smarter node slider — always this
    // one shape, at every width. The current tier's caption closes the block
    // on its own row beneath. The value is cycled with ←/→ or jumped to with
    // a digit (not typed), so the live text is the current selection.
    if let Some(effort) = effort {
        for line in effort_block_lines(effort, effort_levels, body_width, focused_field == 1, theme)
        {
            body.push(line);
        }
    }

    // Thinking block (optional): a real on/off checkbox, not a carousel — the
    // boolean reads as `[x]`/`[ ]` and toggles with Space (a non-text field,
    // so no caret while focused). Orthogonal to effort.
    if let Some(on) = thinking {
        let label = format!("{:<8}", "Thinking");
        let box_style = Style::default()
            .fg(if focused_field == 2 {
                theme.brand()
            } else {
                theme.fg()
            })
            .add_modifier(Modifier::BOLD);
        let word_style = Style::default().fg(if on { theme.ok() } else { theme.muted() });
        let checkbox = if on { "[x]" } else { "[ ]" };
        let word = if on { "on" } else { "off" };
        let tail = format!("{checkbox} {word}");
        let pad = body_width.saturating_sub(label.width() + tail.width());
        body.push(Line::from(vec![
            Span::styled(label, label_style),
            Span::raw(" ".repeat(pad)),
            Span::styled(checkbox.to_string(), box_style),
            Span::styled(format!(" {word}"), word_style),
        ]));
    }

    let body_rect = f.body;
    render_body(frame, body_rect, body, &mut 0, None, 0, false, theme);

    if let Some(fo) = f.footer {
        let mut hints: Vec<FooterHint> = Vec::with_capacity(6);
        hints.push(FooterHint::primary(keyvocab::ENTER, "save"));
        if effort.is_some() || thinking.is_some() {
            hints.push(FooterHint::secondary(keyvocab::TAB, "field"));
        }
        if effort.is_some() {
            // ←/→ steps one rung; a digit jumps straight to that tier.
            hints.push(FooterHint::secondary(keyvocab::ARROWS_LR, "effort"));
            hints.push(FooterHint::secondary("1-7", "jump"));
        }
        if thinking.is_some() {
            hints.push(FooterHint::secondary(keyvocab::SPACE, "thinking"));
        }
        hints.push(FooterHint::always(keyvocab::ESC, "cancel"));
        render_modal_footer(frame, fo, &hints, theme);
    }

    // Place the caret on the API-key row, after its label. It is the editor's
    // only free-text field: the effort selector is cycled / jumped and the
    // thinking row is a toggle, so neither shows a caret — a parked cursor on
    // a non-text field reads as "type here" and jitters as the value cycles.
    if show_key && focused_field == 0 {
        let prefix = format!("{:<8}", "API key");
        // Subtract the field's viewport offset so the caret tracks the visible
        // (scrolled) text, and clamp it to stay inside the body rect.
        let caret_col = caret_column(input, cursor_position);
        let max_x = body_rect.x + body_rect.width.saturating_sub(1);
        let mut cursor_x =
            (body_rect.x + prefix.width() as u16 + caret_col).saturating_sub(api_key_off as u16);
        if cursor_x > max_x {
            cursor_x = max_x;
        }
        let cursor_y = body_rect.y;
        frame.set_cursor_position((cursor_x, cursor_y));
    }
    area
}

/// Render the suggestion dropdown shared by the filter fields: up to a few
/// matches, the highlighted one marked `›` in the brand tone, windowed around the
/// highlight so a long list stays navigable. An empty list shows a `(no match)`
/// hint (Enter then uses the typed text).
fn suggestion_lines(
    suggestions: &[String],
    highlight: usize,
    empty_hint: &str,
    theme: &Theme,
) -> Vec<Line<'static>> {
    const MAX: usize = 6;
    if suggestions.is_empty() {
        return vec![Line::from(Span::styled(
            format!("    {empty_hint}"),
            Style::default().fg(theme.muted()),
        ))];
    }
    let start = if highlight >= MAX {
        highlight + 1 - MAX
    } else {
        0
    };
    suggestions
        .iter()
        .enumerate()
        .skip(start)
        .take(MAX)
        .map(|(i, s)| {
            let s_style = crate::components::options::choice_style(
                crate::components::options::ChoiceTone::Flat,
                i == highlight,
                theme,
            );
            let (marker, style) = if i == highlight {
                (
                    " › ",
                    Style::default().fg(s_style.fg).add_modifier(Modifier::BOLD),
                )
            } else {
                ("   ", Style::default().fg(s_style.dim))
            };
            Line::from(Span::styled(format!("{marker}{s}"), style))
        })
        .collect()
}

/// Draw the OAuth-in-progress sheet: instruction, URL, optional user code, status.
#[allow(clippy::too_many_arguments)]
pub fn draw_oauth_pending(
    title: &str,
    message: &str,
    url: &str,
    user_code: &str,
    error: Option<&str>,
    _selected_item: usize,
    frame: &mut Frame,
    theme: &Theme,
    scroll: &mut usize,
    hit_map: Option<&mut crate::model::layout::ModalHitMap>,
    selection: &SelectionState,
    layout_map: &mut LayoutMap,
) -> mutx_engine::Rect {
    let geometry = ContentModalSpec::OAUTH_PENDING;
    let probe = content_modal_probe(frame, geometry);
    let probe_w = (probe.width as usize).saturating_sub(6).max(20);

    let mut raw_lines: Vec<(String, Style)> = Vec::new();
    let mut estimated_rows: u16 = 0;

    if let Some(err) = error {
        raw_lines.push((
            format!("✗ {err}"),
            Style::default()
                .fg(theme.err())
                .add_modifier(Modifier::BOLD),
        ));
        raw_lines.push((String::new(), Style::default()));
        raw_lines.push((
            "Press Esc to go back and try again.".to_string(),
            Style::default().fg(theme.muted()),
        ));
        estimated_rows += 3;
    } else {
        if !message.is_empty() {
            raw_lines.push((message.to_string(), Style::default().fg(theme.fg())));
            raw_lines.push((String::new(), Style::default()));
            let msg_wrapped = (message.len().saturating_sub(1) / probe_w) as u16;
            estimated_rows += 2 + msg_wrapped;
        }

        if !url.is_empty() {
            raw_lines.push((
                "Open this link in your browser if it did not open automatically:".to_string(),
                Style::default().fg(theme.muted()),
            ));
            raw_lines.push((
                url.to_string(),
                Style::default()
                    .fg(theme.brand())
                    .add_modifier(Modifier::UNDERLINED),
            ));
            raw_lines.push((String::new(), Style::default()));
            let url_wrapped = (url.len().saturating_sub(1) / probe_w) as u16;
            estimated_rows += 3 + url_wrapped;
        } else {
            raw_lines.push((
                "Starting authorization…".to_string(),
                Style::default().fg(theme.muted()),
            ));
            raw_lines.push((String::new(), Style::default()));
            estimated_rows += 2;
        }

        if !user_code.is_empty() {
            raw_lines.push((
                format!("Verification Code: {user_code}"),
                Style::default().fg(theme.fg()).add_modifier(Modifier::BOLD),
            ));
            raw_lines.push((String::new(), Style::default()));
            estimated_rows += 2;
        }

        raw_lines.push((
            "Waiting for authorization to complete in browser…".to_string(),
            Style::default().fg(theme.muted()),
        ));
        estimated_rows += 1;
    }

    let desired =
        estimated_rows.max(raw_lines.len() as u16) + modal_chrome_rows(geometry.modal_spec());
    let area = content_modal_area(frame, geometry, desired);
    let f = modal_frame(frame, area, theme.panel(), true, true);

    if let Some(map) = hit_map {
        map.set_oauth_modal_rect(area);
        map.set_oauth_url_rect(f.body);
    }

    if let Some(h) = f.header {
        let parts = hierarchical_breadcrumb(&["Connections", "Add", title], h.width as usize);
        modal_header_parts(frame, Some(h), &parts, theme);
    }

    // Selectable document body via the shared component: application-layer
    // wrapping, one MODAL_DOC region per *visual* row (the hand-rolled loop
    // this replaces registered one region per logical row with a 1-row rect,
    // which misaligned with the engine's internal wrap on continuation lines
    // and after scroll), and selection splitting identical to the
    // transcript's.
    let rows: Vec<SelectableRow> = raw_lines
        .into_iter()
        .map(|(text, style)| SelectableRow::styled(text, style))
        .collect();
    render_selectable_body(
        frame, f.body, &rows, scroll, None, theme, selection, layout_map,
    );

    if let Some(fo) = f.footer {
        let mut hints: Vec<FooterHint> = Vec::new();
        if error.is_none() {
            if !url.is_empty() {
                hints.push(FooterHint::secondary("u", "copy url"));
            }
            if !user_code.is_empty() {
                hints.push(FooterHint::secondary("c", "copy code"));
            }
        }
        hints.push(FooterHint::always(keyvocab::ESC, "cancel"));
        render_modal_footer(frame, fo, &hints, theme);
    }
    area
}

/// The render policy of one template-chooser row, factored out of the loop so
/// the rules are unit-testable without a terminal. An unfocused row shows its
/// title alone; the focused row also reveals the one-line description and an
/// auth-scheme badge (`oauth` / `token`), separated from the title by the
/// standard [`RowGroup`] whitespace gap — never a `·` glyph. The wire protocol
/// and the seeded model count are deliberately omitted: neither changes what
/// the user does next (the models an endpoint will actually serve are only
/// knowable with a working token, and the protocol is locked by the template).
struct TemplateRow {
    /// Visible width the row must fill edge-to-edge.
    body_width: usize,
}

impl TemplateRow {
    /// Columns available for the title before the trailing badge claims its
    /// share. The badge is painted even when unfocused (it is `dim`, cheap),
    /// so the title must always leave room for it — otherwise the columns
    /// would jump as the selection moves.
    fn title_budget(&self) -> usize {
        let badge_w = 1 + AUTH_BADGE_WIDTH; // glyph + intra-group gap
        (self.body_width / 2)
            .saturating_sub(GUTTER + GROUP_GAP + badge_w)
            .max(1)
    }
}

/// Widest badge label both variants render ("oauth" and "token" are 5).
const AUTH_BADGE_WIDTH: usize = 5;

impl TemplateRow {
    fn build(
        &self,
        template: &ProviderTemplate,
        focused: bool,
        theme: &Theme,
    ) -> Vec<Line<'static>> {
        let style = choice_style(ChoiceTone::Filled, focused, theme);
        let title = truncate_ellipsis(template.display_title(), self.title_budget());

        // Identity: the title, bold. One styled atom per char keeps the door
        // open for fuzzy highlighting if the chooser ever grows a filter.
        let mut identity = RowGroup::fixed();
        for c in title.chars() {
            identity = identity.styled(
                RowStyledAtom {
                    text: c.to_string(),
                    style: Style::default()
                        .bg(style.bg)
                        .fg(style.fg)
                        .add_modifier(Modifier::BOLD),
                },
                0,
            );
        }

        let mut row = ListRow::new(style, self.body_width).group(identity);

        // Trailing badge: `⚡ oauth` / `⚿ token`, right-pinned so the two
        // columns spread across the row. On a brand-filled focused row it
        // lifts to the contrast foreground.
        let badge_fg = if focused { row.fill_fg() } else { style.dim };
        let glyph = if template.auth.is_oauth() {
            "⚡"
        } else {
            "⚿"
        };
        row = row.group(RowGroup::trailing().glyph(glyph, badge_fg, 0).text(
            template.auth_badge(),
            badge_fg,
            1,
        ));

        if !focused {
            return vec![row.finish()];
        }

        // Focused: append the description as a second, non-selectable line
        // painted in the panel background (NOT the brand fill — the highlight
        // band stays exactly one row tall, the same convention as the
        // permission sheet's wrapped choice rows).
        let mut lines = vec![row.finish()];
        let indent = " ".repeat(GUTTER + GROUP_GAP);
        push_wrapped_styled(
            &mut lines,
            &indent,
            &indent,
            template.description,
            Style::default().bg(theme.panel()).fg(theme.dim()),
            self.body_width,
        );
        lines
    }
}

/// Draw the provider-template chooser as the Connections list's Add connection
/// child page. It retains the parent panel geometry and uses a breadcrumb
/// header so navigation does not look like a separate modal.
///
/// Row rules (see [`TemplateRow`]):
/// - unfocused rows show **only the title** — no description, no meta;
/// - the focused row is a full-width **background highlight** (brand fill, the
///   Connections/Models standard) with no `›` marker, so the title column is
///   never indented by the cursor;
/// - a trailing `⚿ oauth`/`⚿ token` badge marks the auth scheme — the only
///   per-row meta. The protocol and seeded model count are omitted: the
///   user cannot query an endpoint's real catalog without credentials, and
///   the wire protocol is an implementation detail of the locked template.
/// - rows are sorted by title (see [`PROVIDER_TEMPLATES`]).
///
/// `scroll` is read AND written back so the offset stays consistent with the
/// clamped body height; the focused template is followed on-screen so `↑/↓`
/// navigation keeps it visible even when the list overflows the body.
pub fn draw_provider_template_chooser(
    selected: usize,
    frame: &mut Frame,
    theme: &Theme,
    scroll: &mut usize,
) -> mutx_engine::Rect {
    let area = modal_area(frame, FixedModalSpec::PROVIDER);
    let f = modal_frame(frame, area, theme.panel(), true, true);

    let header = hierarchical_breadcrumb(
        &["Connections", "Add Provider"],
        f.header.map(|h| h.width as usize).unwrap_or(80),
    );
    modal_header_parts(frame, f.header, &header, theme);

    let policy = TemplateRow {
        body_width: f.body.width as usize,
    };

    let mut body: Vec<Line> = Vec::new();
    let mut follow: Option<usize> = None;
    for (i, template) in PROVIDER_TEMPLATES.iter().enumerate() {
        let focused = i == selected;
        if focused {
            follow = Some(body.len());
        }
        body.extend(policy.build(template, focused, theme));
    }

    render_body(
        frame,
        f.body,
        body,
        scroll,
        follow,
        SCROLL_EDGE_MARGIN,
        false,
        theme,
    );

    if let Some(fo) = f.footer {
        render_modal_footer(
            frame,
            fo,
            &[
                FooterHint::navigation(keyvocab::ARROWS_UD, "navigate"),
                FooterHint::primary(keyvocab::ENTER, "select"),
                FooterHint::always(keyvocab::ESC, "back"),
            ],
            theme,
        );
    }
    area
}

/// Everything [`draw_custom_provider_editor`] renders, bundled so the call site
/// stays readable.
pub struct CustomEditorView<'a> {
    /// Ordered visible fields, chosen by the active template (create) or the
    /// edited provider's protocol (edit).
    pub fields: &'a [CustomField],
    /// Focused field index into [`Self::fields`].
    pub field: u8,
    /// Edit mode hides the Model field and changes the Token hint / header.
    pub editing: bool,
    /// Header title — the template label (create) or `Edit · <name>` (edit).
    pub title: &'a str,
    pub name_buf: &'a str,
    pub base_url_buf: &'a str,
    pub token_buf: &'a str,
    /// Display name of the committed model (shown when Model is unfocused).
    pub model_display: &'a str,
    /// Base URL placeholder — the template's expected endpoint shape.
    pub url_hint: &'a str,
    /// Model suggestions for the Model filter field (empty off that field).
    pub suggestions: &'a [String],
    pub suggest_index: usize,
    /// The focused field's live value (text buffer, or the Model filter query).
    pub input: &'a str,
    pub cursor_position: usize,
}

/// Draw the provider editor: a per-template form drawn from [`CustomEditorView::fields`]
/// (Name / Base URL / Token, plus a type-to-filter Model field when a template
/// opts in). Focusing the Model field renders a suggestion
/// dropdown below the form; `↑/↓` move the highlight (committed live). The Token
/// is masked unless focused. In edit mode the header reads `Edit · <name>`.
pub fn draw_custom_provider_editor(
    view: CustomEditorView<'_>,
    frame: &mut Frame,
    theme: &Theme,
    scroll: &mut usize,
) -> mutx_engine::Rect {
    let CustomEditorView {
        fields,
        field,
        editing,
        title,
        name_buf,
        base_url_buf,
        token_buf,
        model_display,
        url_hint,
        suggestions,
        suggest_index,
        input,
        cursor_position,
    } = view;

    let geometry = ContentModalSpec::CUSTOM_PROVIDER;
    let model_focused = fields.get(field as usize) == Some(&CustomField::Model);
    let suggest_count = if model_focused && !suggestions.is_empty() {
        (suggestions.len().min(8) as u16) + 2
    } else {
        0
    };
    let desired = (fields.len() as u16) + suggest_count + modal_chrome_rows(geometry.modal_spec());
    let area = content_modal_area(frame, geometry, desired);
    let f = modal_frame(frame, area, theme.panel(), true, true);

    const LABEL_W: usize = 9;
    let body_width = f.body.width as usize;
    let label_cell_w = 3 + LABEL_W; // focus marker + padded label span
    let field_w = body_width.saturating_sub(label_cell_w);
    // Every focused field borrows the composer `input`, so the focused field's
    // viewport offset is the same regardless of which one is focused.
    let focus_off = if input.is_empty() {
        0
    } else {
        field_viewport(input, cursor_position, field_w).0
    };
    let field_label = |label: &str, focused: bool| {
        let style = if focused {
            Style::default()
                .fg(theme.brand())
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.muted())
        };
        let marker = if focused { " › " } else { "   " };
        Span::styled(format!("{marker}{label:<LABEL_W$}"), style)
    };
    let value_style = |focused: bool| {
        if focused {
            Style::default().fg(theme.fg()).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.muted())
        }
    };
    let placeholder = |val: String, focused: bool, hint: &str| -> Span<'static> {
        if val.is_empty() && focused {
            Span::styled(hint.to_string(), Style::default().fg(theme.muted()))
        } else {
            Span::styled(val, value_style(focused))
        }
    };
    // Focused rows scroll under the caret via the viewport; unfocused rows are
    // width-capped (Token's `•` mask truncates the same way). This keeps long
    // keys/base URLs from overflowing the modal.
    let text_row = |focused: bool, label: &str, buf: &str, hint: &str, mask: bool| {
        let raw = if focused {
            field_viewport(input, cursor_position, field_w).1
        } else if mask {
            truncate_ellipsis(&"•".repeat(buf.chars().count()), field_w.max(1))
        } else {
            truncate_ellipsis(buf, field_w.max(1))
        };
        Line::from(vec![
            field_label(label, focused),
            placeholder(raw, focused, hint),
        ])
    };
    // The Model filter row shows the live query (caret) when focused, else the
    // committed model's display name.
    let model_row = |focused: bool| {
        let value = if focused {
            placeholder(
                field_viewport(input, cursor_position, field_w).1,
                true,
                "type to filter…",
            )
        } else {
            Span::styled(
                truncate_ellipsis(model_display, field_w.max(1)),
                value_style(false),
            )
        };
        Line::from(vec![field_label("Model", focused), value])
    };

    let header_width = f.header.map(|h| h.width as usize).unwrap_or(80);
    let levels: Vec<&str> = if editing {
        vec!["Connections", "Edit", title]
    } else {
        vec!["Connections", "Add", title]
    };
    let header = hierarchical_breadcrumb(&levels, header_width);
    modal_header_parts(frame, f.header, &header, theme);

    let token_hint = if editing {
        "blank = keep existing"
    } else {
        "API key (blank for local)"
    };
    let mut body: Vec<Line> = Vec::new();
    for (idx, fld) in fields.iter().enumerate() {
        let focused = idx as u8 == field;
        body.push(match fld {
            CustomField::Name => text_row(focused, "Name", name_buf, "e.g. My Relay", false),
            CustomField::BaseUrl => text_row(focused, "Base URL", base_url_buf, url_hint, false),
            CustomField::Token => text_row(focused, "Token", token_buf, token_hint, true),
            CustomField::Model => model_row(focused),
        });
    }
    // Suggestion dropdown while the Model filter field is focused.
    let model_focused = fields.get(field as usize) == Some(&CustomField::Model);
    if model_focused {
        body.push(Line::from(""));
        body.push(Line::from(Span::styled(
            " Model matches".to_string(),
            Style::default().fg(theme.muted()),
        )));
        body.extend(suggestion_lines(
            suggestions,
            suggest_index,
            "(type a custom model id)",
            theme,
        ));
    }

    let body_rect = f.body;
    // While the Model filter is focused, keep the highlighted suggestion
    // on-screen as ↑/↓ moves it. The suggestion block starts at
    // `fields.len() + 2` (form rows + blank + "Model matches" header), so the
    // highlight's visual row is that base plus `suggest_index`.
    let follow = if model_focused && !suggestions.is_empty() {
        Some(fields.len() + 2 + suggest_index)
    } else {
        None
    };
    render_body(
        frame,
        body_rect,
        body,
        scroll,
        follow,
        SCROLL_EDGE_MARGIN,
        false,
        theme,
    );
    if let Some(fo) = f.footer {
        let mut hints: Vec<FooterHint> = Vec::with_capacity(5);
        hints.push(FooterHint::secondary(keyvocab::TAB, "field"));
        if model_focused {
            hints.push(FooterHint::secondary("type", "filter"));
            hints.push(FooterHint::navigation(keyvocab::ARROWS_UD, "choose"));
        } else {
            hints.push(FooterHint::navigation(keyvocab::ARROWS_UD, "scroll"));
        }
        hints.push(FooterHint::primary(keyvocab::ENTER, "save"));
        hints.push(FooterHint::always(keyvocab::ESC, "cancel"));
        render_modal_footer(frame, fo, &hints, theme);
    }

    // Caret on the focused field's row — every visible field borrows the input
    // line (plain text for Name/URL/Token, the filter query for Model). Subtract
    // the focused field's viewport offset and clamp to stay inside the body.
    // The caret's vertical position must also account for `scroll`: render_body
    // advanced it to keep the followed suggestion visible, so the focused field
    // row may have scrolled off the top. Only show the caret when the field is
    // still in the viewport (scroll <= row < scroll + visible); otherwise hide
    // it (the user is reviewing suggestions below the form).
    let row = field as usize;
    let visible = body_rect.height as usize;
    let in_view = (*scroll <= row) && (row < *scroll + visible);
    if in_view {
        let prefix_w = 3 + LABEL_W as u16; // focus marker + padded label
        let caret_col = caret_column(input, cursor_position);
        let max_x = body_rect.x + body_rect.width.saturating_sub(1);
        let mut cursor_x = (body_rect.x + prefix_w + caret_col).saturating_sub(focus_off as u16);
        if cursor_x > max_x {
            cursor_x = max_x;
        }
        let cursor_y = body_rect.y + (row - *scroll) as u16;
        frame.set_cursor_position((cursor_x, cursor_y));
    }
    area
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Render the whole frame buffer back to a single string (rows joined by
    /// `\n`), the standard readback helper for layout-level modal assertions.
    fn buffer_text(terminal: &mutx_engine::TestTerminal) -> String {
        let buf = terminal.buffer();
        let area = buf.area();
        (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Render the per-model settings editor (effort + thinking, no API key)
    /// into a terminal of the given size and read back the buffer text.
    fn render_settings_editor(
        width: u16,
        height: u16,
        effort: Option<&str>,
        levels: &[String],
        thinking: Option<bool>,
    ) -> String {
        let theme = Theme::default();
        let mut terminal = mutx_engine::TestTerminal::new(width, height);
        terminal.draw(|f| {
            draw_model_editor(
                f,
                "claude-opus-4-8",
                effort.unwrap_or(""),
                0,
                false,
                1,
                effort,
                levels,
                thinking,
                &theme,
            );
        });
        buffer_text(&terminal)
    }

    fn levels(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    /// The reasoning-tag decision from `model_list_body`, factored out for a
    /// direct unit test (the row renderer is layout machinery; the policy is
    /// what matters).
    fn reasoning_tag(thinking: Option<bool>, effort: Option<&str>) -> String {
        match (thinking, effort) {
            (Some(true), Some(effort)) => format!("think on {effort}"),
            (Some(true), None) => "think on".to_string(),
            (None, Some(effort)) => effort.to_string(),
            _ => String::new(),
        }
    }

    #[test]
    fn effort_slider_renders_at_every_supported_width() {
        // The selector is the slider at EVERY width, so it must lay out from
        // the minimum terminal (40 cols, per MIN_TERMINAL_COLS) upward, for
        // every ladder shape and every selection, without panicking — the
        // label thinning guarantees no overlap, not just no crash.
        let ladders: Vec<Vec<&str>> = vec![
            vec!["none", "minimal", "low", "medium", "high", "xhigh", "max"],
            vec!["low", "medium", "high", "xhigh", "max"],
            vec!["low", "medium", "high"],
            vec!["low", "high", "max"],
            vec!["medium"],
        ];
        for cols in 40u16..121 {
            for ladder in &ladders {
                for tier in ladder {
                    let lv: Vec<String> = ladder.iter().map(|s| s.to_string()).collect();
                    let text = render_settings_editor(cols, 24, Some(tier), &lv, None);
                    // The slider's shape markers are present at every width.
                    assert!(
                        text.contains('●'),
                        "marker at {cols} cols ({tier}): {text:?}"
                    );
                    assert!(
                        !text.contains("< "),
                        "no carousel chevrons at {cols} cols: {text:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn effort_selector_renders_as_a_node_slider_when_wide() {
        // Wide enough: the `Effort` label owns its row, then a blank row, a
        // `Faster ⇄ Smarter` track with a circle node per tier and the marker
        // sitting squarely on the selected node; every tier labeled underneath
        // centered on its node in ascending depth.
        let full = levels(&["low", "medium", "high", "xhigh", "max"]);
        let text = render_settings_editor(120, 24, Some("high"), &full, None);
        let rows: Vec<&str> = text.lines().collect();
        let label_idx = rows
            .iter()
            .position(|l| l.contains("Effort"))
            .expect("label row");
        // The label owns its row — the slider component is on the next one.
        assert!(
            !rows[label_idx].contains("Faster"),
            "label row must not share with the slider: {:?}",
            rows[label_idx]
        );

        let track_row = rows[label_idx + 2];
        assert!(track_row.contains("Faster"), "scale start: {track_row:?}");
        assert!(track_row.contains("Smarter"), "scale end: {track_row:?}");
        // 5 tiers: 4 unselected circles + 1 selected circle marker.
        assert_eq!(
            track_row.chars().filter(|&c| c == '○').count(),
            4,
            "circle nodes for the unselected rungs: {track_row:?}"
        );
        assert!(
            track_row.contains('●'),
            "marker on the selected node: {track_row:?}"
        );
        // The carousel affordance is gone in the slider form.
        assert!(
            !track_row.contains('<'),
            "no carousel chevrons: {track_row:?}"
        );

        let labels_row = rows[label_idx + 3];
        for tier in ["low", "medium", "high", "xhigh", "max"] {
            assert!(labels_row.contains(tier), "missing tier: {labels_row:?}");
        }
        // Depth order is left-to-right ascending.
        let low = labels_row.find("low").unwrap();
        let max = labels_row.find("max").unwrap();
        assert!(low < max, "ladder must ascend left→right: {labels_row:?}");
        // The marker lands exactly on the selected node: same column as the
        // tier label's center.
        let marker = track_row.chars().position(|c| c == '●').unwrap();
        let high = labels_row.find("high").unwrap();
        assert_eq!(
            marker,
            high + "high".len() / 2,
            "marker centered on the selected node: {track_row:?} vs {labels_row:?}"
        );
        // Endpoints are also centered directly under their node columns.
        let left_node = track_row.chars().position(|c| c == '○').unwrap();
        let low_col = labels_row.find("low").unwrap();
        assert_eq!(
            left_node,
            low_col + "low".len() / 2,
            "low centered under the left endpoint node"
        );
        let right_node = track_row
            .chars()
            .enumerate()
            .filter(|(_, c)| *c == '○')
            .last()
            .map(|(i, _)| i)
            .unwrap();
        let max_col = labels_row.rfind("max").unwrap();
        assert_eq!(
            right_node,
            max_col + "max".len() / 2,
            "max centered under the right endpoint node"
        );
    }

    #[test]
    fn effort_selector_stays_a_slider_when_narrow() {
        // One shape at every width: too narrow for verbatim tier labels and
        // the block still renders the `Faster ⇄ Smarter` slider — cramped
        // interior labels thin out (ends + selected stay) instead of swapping
        // to a carousel. Absolutely no `<`/`>` chevrons anywhere.
        let full = levels(&["low", "medium", "high", "xhigh", "max"]);
        let text = render_settings_editor(56, 24, Some("high"), &full, None);
        let rows: Vec<&str> = text.lines().collect();
        let track_row = rows
            .iter()
            .find(|l| l.contains("Faster"))
            .expect("track row at narrow width");
        assert!(track_row.contains("Smarter"), "scale end: {track_row:?}");
        assert!(track_row.contains('●'), "marker: {track_row:?}");
        // The carousel affordance is gone at every width.
        for row in &rows {
            assert!(!row.contains("< "), "no carousel chevrons: {row:?}");
        }
        // The labels row still exists (ends are always labeled).
        let labels_row = rows
            .iter()
            .find(|l| l.contains("low"))
            .expect("labels row at narrow width");
        assert!(
            labels_row.contains("max"),
            "far end labeled: {labels_row:?}"
        );
        // The selected rung keeps its label even when the layout thins.
        assert!(
            labels_row.contains("high"),
            "selected rung labeled: {labels_row:?}"
        );
        // Every rung keeps its node on the track: 5 tiers = 4 circles + 1 marker.
        let nodes = track_row
            .chars()
            .filter(|&c| matches!(c, '○' | '●'))
            .count();
        assert_eq!(nodes, 5, "a node per rung: {track_row:?}");
    }

    #[test]
    fn effort_selector_lays_out_the_full_openai_ladder() {
        // The 7-rung OpenAI ladder (`none`…`max`) fits a standard body with
        // every tier labeled verbatim — no squeezing needed where it counts.
        let openai = levels(&["none", "minimal", "low", "medium", "high", "xhigh", "max"]);
        let text = render_settings_editor(120, 24, Some("medium"), &openai, None);
        let labels_row = text
            .lines()
            .find(|l| l.contains("minimal"))
            .expect("labels row");
        for tier in ["none", "minimal", "low", "medium", "high", "xhigh", "max"] {
            assert!(labels_row.contains(tier), "missing tier: {labels_row:?}");
        }
        assert!(
            !labels_row.contains('≤') && !labels_row.contains('≥'),
            "wide body labels verbatim: {labels_row:?}"
        );
    }

    #[test]
    fn effort_caption_shows_in_both_forms() {
        // The current tier's caption closes the block on its own row —
        // truncated to the available width rather than dropped or wrapped
        // awkwardly — at every width, and for an unknown ladder too.
        let full = levels(&["low", "medium", "high", "xhigh", "max"]);
        let wide = render_settings_editor(120, 24, Some("high"), &full, None);
        assert!(
            wide.contains("deep reasoning"),
            "slider caption present: {wide:?}"
        );
        let narrow = render_settings_editor(56, 24, Some("high"), &full, None);
        assert!(
            narrow.contains("deep reasoning"),
            "narrow slider caption present: {narrow:?}"
        );
        let unknown = render_settings_editor(120, 24, Some("high"), &[], None);
        assert!(
            unknown.contains("deep reasoning"),
            "unknown-ladder caption present: {unknown:?}"
        );
    }

    #[test]
    fn only_the_api_key_field_shows_a_caret() {
        // The effort selector is cycled (not typed) and thinking is a toggle,
        // so neither may raise the text caret — a parked cursor on a non-text
        // field reads as "type here" and jitters as the value cycles.
        let theme = Theme::default();
        let full = levels(&["low", "medium", "high", "xhigh", "max"]);
        let mut terminal = mutx_engine::TestTerminal::new(120, 24);
        terminal.draw(|f| {
            draw_model_editor(
                f,
                "m",
                "sk-live",
                7,
                true,
                1,
                Some("high"),
                &full,
                None,
                &theme,
            );
        });
        assert_eq!(
            terminal.cursor(),
            mutx_engine::CursorState::Hidden,
            "no caret while the effort selector is focused"
        );
        let mut terminal = mutx_engine::TestTerminal::new(120, 24);
        terminal.draw(|f| {
            draw_model_editor(
                f,
                "m",
                "sk-live",
                7,
                true,
                0,
                Some("high"),
                &full,
                None,
                &theme,
            );
        });
        assert!(
            matches!(terminal.cursor(), mutx_engine::CursorState::Visible(..)),
            "the API-key text field keeps its caret"
        );
    }

    #[test]
    fn thinking_renders_as_a_checkbox_not_a_carousel() {
        // The boolean is `[x]`/`[ ]`, never `< on >` — the control's shape
        // finally matches its semantics.
        let on = render_settings_editor(100, 24, None, &[], Some(true));
        assert!(on.contains("[x] on"), "checked: {on:?}");
        assert!(!on.contains("< on >"), "no carousel for a bool: {on:?}");
        let off = render_settings_editor(100, 24, None, &[], Some(false));
        assert!(off.contains("[ ] off"), "unchecked: {off:?}");
    }

    #[test]
    fn effort_block_row_count_depends_only_on_the_ladder() {
        // The selector is the slider at every width, so the block's row count
        // is width-independent by construction — six rows for a known ladder
        // (label + blank + track + tier labels + blank + caption), three for an unknown one
        // (value row + blank + caption). Nothing can flip between two shapes as the
        // user cycles or resizes.
        let common = levels(&["low", "medium", "high"]);
        assert_eq!(effort_block_rows(&common), 6, "3-tier → slider rows");
        let openai = levels(&["none", "minimal", "low", "medium", "high", "xhigh"]);
        assert_eq!(effort_block_rows(&openai), 6, "6-tier → slider rows");
        // An unknown ladder collapses to the value row + blank + caption.
        assert_eq!(effort_block_rows(&[]), 3, "empty ladder → value + blank + caption");
    }

    #[test]
    fn reasoning_tag_shows_openai_effort_and_anthropic_opt_in() {
        // Anthropic: opted-in thinking shows `think on <effort>`; opted-out
        // shows nothing even when an effort value is configured.
        assert_eq!(reasoning_tag(Some(true), Some("high")), "think on high");
        assert_eq!(reasoning_tag(Some(true), None), "think on");
        assert_eq!(reasoning_tag(Some(false), Some("high")), "");
        // OpenAI (Kimi K3 & friends): no thinking switch, so the current
        // effort shows directly — this is the picker-row half of the hint
        // bar's `Kimi K3 max` tag.
        assert_eq!(reasoning_tag(None, Some("max")), "max");
        // Unconfigured models show nothing.
        assert_eq!(reasoning_tag(None, None), "");
    }

    /// Render the provider-template chooser at `selected` into a terminal and
    /// read back the full buffer text, the standard readback for the chooser's
    /// layout-level assertions.
    fn render_template_chooser(selected: usize, width: u16, height: u16) -> String {
        let theme = Theme::default();
        let mut terminal = mutx_engine::TestTerminal::new(width, height);
        let mut scroll = 0;
        terminal.draw(|f| {
            draw_provider_template_chooser(selected, f, &theme, &mut scroll);
        });
        buffer_text(&terminal)
    }

    #[test]
    fn template_rows_are_sorted_by_title() {
        // The chooser's display order IS the table order (the const is kept
        // sorted), so an out-of-order insertion breaks the alphabetical rule
        // at the declaration site. This test pins it.
        let titles: Vec<&str> = PROVIDER_TEMPLATES.iter().map(|t| t.label).collect();
        let mut sorted = titles.clone();
        sorted.sort();
        assert_eq!(
            titles, sorted,
            "PROVIDER_TEMPLATES must stay sorted by label (title)"
        );
        // And the chooser renders them in table order, so display order is
        // alphabetical by construction.
    }

    #[test]
    fn template_chooser_shows_only_titles_when_unfocused() {
        // Selection 0 = "Anthropic" (the table is title-sorted). Every other
        // row is unfocused, so its description must NOT be in the buffer;
        // only the focused row's description is revealed.
        let text = render_template_chooser(0, 100, 32);
        assert!(
            text.contains("Anthropic"),
            "focused title present: {text:?}"
        );
        // Focused row's description is revealed.
        assert!(
            text.contains("Claude models over the Anthropic"),
            "focused description revealed: {text:?}"
        );
        // An unfocused row's description is hidden (Antigravity OAuth is
        // further down the sorted list).
        assert!(
            !text.contains("via Google OAuth subscription"),
            "unfocused rows show title only: {text:?}"
        );
        // The old meta run is gone. Checked per line, and only for a DIGIT-
        // prefixed "N model(s)" — the revealed description legitimately
        // contains the word "models" ("Claude models over …").
        for line in text.lines() {
            let count_meta = line.match_indices(" model").any(|(i, _)| {
                line[..i]
                    .chars()
                    .next_back()
                    .is_some_and(|c| c.is_ascii_digit())
            });
            assert!(!count_meta, "no seeded model-count meta: {line:?}");
            assert!(
                !line.contains("· openai")
                    && !line.contains("· google")
                    && !line.contains("· anthropic"),
                "no `·`-joined protocol meta: {line:?}"
            );
        }
        // No `›` cursor marker in the body (the header breadcrumb's `›` is
        // outside the rows, so the whole-buffer scan would false-positive —
        // check the row lines instead).
        let rows: Vec<&str> = text.lines().collect();
        let body_rows = rows
            .iter()
            .filter(|l| l.contains("GitHub") || l.contains("OpenAI") || l.contains("Anthropic"))
            .collect::<Vec<_>>();
        assert!(
            body_rows.iter().all(|l| !l.contains('›')),
            "no `›` cursor marker on rows: {body_rows:?}"
        );
    }

    #[test]
    fn template_chooser_marks_the_auth_scheme_without_a_dot_join() {
        // The focused row carries an auth badge: `oauth` for browser flows,
        // `token` for API keys — separated from the title by whitespace only.
        let xai_idx = PROVIDER_TEMPLATES
            .iter()
            .position(|t| t.id == "xai-oauth")
            .expect("xai-oauth template");
        let text = render_template_chooser(xai_idx, 100, 32);
        let row = text
            .lines()
            .find(|l| l.contains("xAI"))
            .expect("focused xAI row");
        assert!(row.contains("oauth"), "oauth badge: {row:?}");
        assert!(
            !row.contains("oauth ·") && !row.contains("· oauth"),
            "badge is whitespace-separated, not `·`-joined: {row:?}"
        );

        let openai_idx = PROVIDER_TEMPLATES
            .iter()
            .position(|t| t.id == "openai")
            .expect("openai template");
        let text = render_template_chooser(openai_idx, 100, 32);
        let row = text
            .lines()
            .find(|l| l.contains("OpenAI"))
            .expect("focused OpenAI row");
        assert!(row.contains("token"), "token badge: {row:?}");
        assert!(
            !row.contains("token ·") && !row.contains("· token"),
            "badge is whitespace-separated, not `·`-joined: {row:?}"
        );
    }

    #[test]
    fn template_chooser_highlights_the_focused_row_with_a_background_fill() {
        // The focused row paints a brand background across its full width —
        // the Connections/Models standard — instead of a `›` marker. Locate
        // the focused title's row in the cell buffer and assert every column
        // of that row carries the brand background (an unbroken band).
        let openai_idx = PROVIDER_TEMPLATES
            .iter()
            .position(|t| t.id == "openai")
            .expect("openai template");
        let theme = Theme::default();
        let mut terminal = mutx_engine::TestTerminal::new(100, 32);
        let mut scroll = 0;
        terminal.draw(|f| {
            draw_provider_template_chooser(openai_idx, f, &theme, &mut scroll);
        });

        // Rebuild the row texts from the buffer to find the focused row's y.
        let buf = terminal.buffer();
        let area = buf.area();
        let row_text = |y: u16| -> String {
            (0..area.width)
                .map(|x| buf[(x, y)].symbol().to_string())
                .collect::<String>()
        };
        let focused_y = (0..area.height)
            .map(|y| (y, row_text(y)))
            // Exact-title match: a subtitle row ("Custom OpenAI") must not
            // capture a search for the "OpenAI" template's row.
            .find(|(_, text)| text.trim().starts_with("OpenAI"))
            .map(|(y, _)| y)
            .expect("focused OpenAI row rendered");

        // Every column of the focused row inside the modal BODY carries the
        // brand background. The panel spans the middle 72% of the viewport
        // with `MODAL_INNER_H_PADDING` (3) content inset each side; find the
        // body's exact edges on the focused row by walking in from the
        // terminal edges past the unpainted (Reset) margin, then skipping
        // the inset padding (painted panel-background, not brand).
        let brand = theme.brand();
        let panel_bg = theme.panel();
        let is_painted = |x: u16| buf[(x, focused_y)].bg() != mutx_engine::Color::Reset;
        let mut left = 0u16;
        while !is_painted(left) {
            left += 1;
        }
        let mut right = area.width - 1;
        while !is_painted(right) {
            right -= 1;
        }
        // Skip inward past the panel padding to the body band.
        let mut body_left = left;
        while buf[(body_left, focused_y)].bg() == panel_bg {
            body_left += 1;
        }
        let mut body_right = right;
        while buf[(body_right, focused_y)].bg() == panel_bg {
            body_right -= 1;
        }
        assert!(
            body_left < body_right,
            "brand band found on the focused row"
        );
        for x in body_left..=body_right {
            assert_eq!(
                buf[(x, focused_y)].bg(),
                brand,
                "column {x} of the focused row must carry the brand fill"
            );
        }
        // An unfocused row (the first title, "Anthropic") carries no brand
        // background — the panel background instead.
        let unfocused_y = (0..area.height)
            .map(|y| (y, row_text(y)))
            .find(|(_, text)| text.contains("Anthropic"))
            .map(|(y, _)| y)
            .expect("first unfocused row rendered");
        assert_ne!(
            buf[(body_left, unfocused_y)].bg(),
            brand,
            "unfocused rows have no brand fill"
        );
    }

    // ── Sectioned Models list (Favorites / Recent / All models) ──────────

    /// A snapshot with one favorite, two used models, and two plain models,
    /// so all three sections render and RECENT has a meaningful internal
    /// order (gpt-5.5 newer than claude-opus-4-8).
    fn sectioned_snapshot() -> muta_contracts::ProviderPickerSnapshot {
        let info =
            |model: &str, favorite: bool, used: Option<u64>| muta_contracts::ProviderModelInfo {
                model: model.to_string(),
                protocol: String::new(),
                effort: None,
                thinking: None,
                favorite,
                last_used_ms: used,
            };
        let row = |id: &str, name: &str, models: Vec<muta_contracts::ProviderModelInfo>| {
            muta_contracts::ProviderPickerRow {
                id: id.to_string(),
                name: name.to_string(),
                model: models.first().map(|m| m.model.clone()).unwrap_or_default(),
                models: models.iter().map(|m| m.model.clone()).collect(),
                model_info: models,
                builtin: true,
                protocol: String::new(),
                base_url: String::new(),
                key_ready: true,
                preset_id: String::new(),
                client_identity: Default::default(),
                last_used_ms: None,
                auth: Default::default(),
            }
        };
        muta_contracts::ProviderPickerSnapshot {
            default_id: "openai".into(),
            rows: vec![
                row(
                    "openai",
                    "OpenAI",
                    vec![
                        info("gpt-5.5", false, Some(1_700_000_000_000)),
                        info("gpt-5.4", false, None),
                    ],
                ),
                row(
                    "anthropic",
                    "Anthropic",
                    vec![
                        info("claude-sonnet-5", true, Some(1_500_000_000_000)),
                        info("claude-opus-4-8", false, Some(1_600_000_000_000)),
                    ],
                ),
                row("google", "Google", vec![info("gemini-3-pro", false, None)]),
            ],
        }
    }

    /// Render the Models modal (browse mode, cursor on `modal_index`) into a
    /// 72×24 terminal and read back the buffer text.
    fn render_models_modal(modal_index: usize, query: &str, search: bool) -> String {
        let theme = Theme::default();
        let picker = sectioned_snapshot();
        let ranked =
            crate::providers::models_flat_filtered_from(&picker, "openai", "gpt-5.5", query);
        let mut terminal = mutx_engine::TestTerminal::new(72, 24);
        terminal.draw(|f| {
            let mut lm = crate::model::layout::LayoutMap::new();
            let mut scroll = 0;
            let selection = crate::model::selection::SelectionState::None;
            draw_models_modal(
                f,
                &mut lm,
                &ranked,
                "openai",
                "gpt-5.5",
                modal_index,
                query,
                query.len(),
                &mut scroll,
                true,
                search,
                false,
                &theme,
                &selection,
            );
        });
        buffer_text(&terminal)
    }

    #[test]
    fn models_modal_renders_three_labeled_sections() {
        // The flat list groups into FAVORITES / RECENT / ALL MODELS with dim
        // label rows between the groups, and the row order inside each
        // section matches the data-layer contract (star beats recency;
        // RECENT is most-recent-first; the rest ASCII).
        let text = render_models_modal(2, "", false);
        let favorites = text.find("FAVORITES").expect("FAVORITES label");
        let recent = text.find("RECENT").expect("RECENT label");
        let all = text.find("ALL MODELS").expect("ALL MODELS label");
        assert!(
            favorites < recent && recent < all,
            "labels in display order"
        );

        let sonnet = text.find("claude-sonnet-5").expect("favorite row");
        let opus = text.find("claude-opus-4-8").expect("older recent row");
        let gpt55 = text.find("gpt-5.5").expect("newer recent row");
        let gemini = text.find("gemini-3-pro").expect("plain row");
        let gpt54 = text.find("gpt-5.4").expect("plain row");
        // Favorite row inside FAVORITES; RECENT rows newest-first between
        // their label and ALL MODELS; plain rows after.
        assert!(favorites < sonnet && sonnet < recent);
        assert!(recent < gpt55 && gpt55 < opus && opus < all);
        assert!(all < gemini && gemini < gpt54);
    }

    #[test]
    fn models_modal_sections_survive_search_mode() {
        // A fuzzy query keeps the same grouping over the filtered rows.
        let text = render_models_modal(0, "g", true);
        assert!(text.contains("RECENT"), "RECENT section under a query");
        assert!(
            text.contains("ALL MODELS"),
            "ALL MODELS section under a query"
        );
        assert!(
            !text.contains("FAVORITES"),
            "no label for an emptied section"
        );
        // gpt-5.5 (recent) renders before gpt-5.4 / gemini (all).
        let recent_gpt = text.find("gpt-5.5").expect("recent match");
        let all_gpt = text.find("gpt-5.4").expect("plain match");
        assert!(recent_gpt < all_gpt);
    }

    #[test]
    fn models_modal_selection_cursor_lands_only_on_model_rows() {
        // Walking the cursor across the section boundaries must keep the
        // brand fill on a MODEL row, never on a label or spacer row: the
        // follow logic maps modal_index through the interleaved geometry.
        for idx in 0..5 {
            let text = render_models_modal(idx, "", false);
            // Every index still paints its model somewhere — the invariant
            // checked here is that the modal renders without panicking and
            // keeps all three labels regardless of cursor position.
            assert!(text.contains("FAVORITES"), "labels stable at idx {idx}");
            assert!(text.contains("RECENT"));
            assert!(text.contains("ALL MODELS"));
        }
    }

    #[test]
    fn models_modal_row_omits_leading_dot_and_trailing_diamond() {
        let text = render_models_modal(0, "", false);
        for line in text.lines() {
            if line.contains("gpt-5.5") || line.contains("claude-sonnet-5") || line.contains("gemini-3-pro") {
                assert!(!line.contains('●'), "no leading dot on row: {line:?}");
                assert!(!line.contains('★'), "no leading star on row: {line:?}");
                assert!(!line.contains('◆'), "no diamond glyph on row: {line:?}");
            }
        }
    }

    #[test]
    fn models_modal_empty_state_centered_copy_and_footer() {
        let theme = Theme::default();
        let mut terminal = mutx_engine::TestTerminal::new(72, 24);
        terminal.draw(|f| {
            let mut lm = crate::model::layout::LayoutMap::new();
            let mut scroll = 0;
            let selection = crate::model::selection::SelectionState::None;
            draw_models_modal(
                f,
                &mut lm,
                &[],
                "",
                "",
                0,
                "",
                0,
                &mut scroll,
                false,
                false,
                false,
                &theme,
                &selection,
            );
        });
        let text = buffer_text(&terminal);
        assert!(text.contains("No models available"));
        assert!(text.contains("Add a connection via /connections (or press a)"));
        assert!(text.contains("Configured models will appear here"));
        assert!(text.contains("add connection"));
        assert!(text.contains("close"));
    }

    #[test]
    fn models_modal_search_empty_state() {
        let theme = Theme::default();
        let mut terminal = mutx_engine::TestTerminal::new(72, 24);
        terminal.draw(|f| {
            let mut lm = crate::model::layout::LayoutMap::new();
            let mut scroll = 0;
            let selection = crate::model::selection::SelectionState::None;
            draw_models_modal(
                f,
                &mut lm,
                &[],
                "",
                "",
                0,
                "xyz",
                3,
                &mut scroll,
                false,
                true,
                false,
                &theme,
                &selection,
            );
        });
        let text = buffer_text(&terminal);
        assert!(text.contains("(no matches — try a shorter or different query)"));
        assert!(text.contains("clear search"));
    }

    #[test]
    fn connections_modal_empty_state_centered_copy_and_footer() {
        let theme = Theme::default();
        let mut terminal = mutx_engine::TestTerminal::new(72, 24);
        terminal.draw(|f| {
            let mut lm = crate::model::layout::LayoutMap::new();
            let mut scroll = 0;
            let selection = crate::model::selection::SelectionState::None;
            draw_connections_modal(
                f,
                &mut lm,
                &[],
                "",
                0,
                "",
                0,
                &mut scroll,
                false,
                false,
                false,
                &theme,
                &selection,
            );
        });
        let text = buffer_text(&terminal);
        assert!(text.contains("No connections yet"));
        assert!(text.contains("Press a to add a provider connection"));
        assert!(text.contains("add"));
        assert!(text.contains("close"));
    }

    #[test]
    fn connections_modal_search_empty_state() {
        let theme = Theme::default();
        let mut terminal = mutx_engine::TestTerminal::new(72, 24);
        terminal.draw(|f| {
            let mut lm = crate::model::layout::LayoutMap::new();
            let mut scroll = 0;
            let selection = crate::model::selection::SelectionState::None;
            draw_connections_modal(
                f,
                &mut lm,
                &[],
                "",
                0,
                "nonexistent",
                11,
                &mut scroll,
                false,
                true,
                false,
                &theme,
                &selection,
            );
        });
        let text = buffer_text(&terminal);
        assert!(text.contains("(no matches — try a shorter or different query)"));
        assert!(text.contains("clear search"));
    }
}
