//! The Connections (provider-instance management) and Models (flat
//! provider/model picker) modals, the API-key / model-id editor, and the
//! custom-provider editor modals.

use neenee_tui_engine::{
    Frame, Paragraph, Rect, {Line, Span}, {Modifier, Style},
};
use unicode_width::UnicodeWidthStr;

use crate::tui::model::layout::LayoutMap;

use super::common::{caret_column, field_viewport, truncate_ellipsis};
use crate::tui::primitives::{
    ContentModalSpec, FixedModalSpec, FooterHint, FooterHintWithBand, SCROLL_EDGE_MARGIN,
    breadcrumb_parts, content_modal_area, content_modal_probe, keymap_body_lines,
    keymap_page_footer_hints, keyvocab, modal_area, modal_chrome_rows, modal_frame, modal_header,
    modal_header_parts, render_body, render_modal_footer, render_modal_footer_with_more,
};
use crate::tui::providers::{CustomField, PROVIDER_TEMPLATES, RankedModel, RankedProvider};
use crate::tui::view::Theme;

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
    _layout_map: &mut LayoutMap,
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
) -> neenee_tui_engine::Rect {
    let area = modal_area(frame, FixedModalSpec::PROVIDER);
    let f = modal_frame(frame, area, theme.panel(), true, true);

    let header_rect = f.header;

    // `a add` opens the template chooser and is the primary action in this view
    // (rank 80), surviving width collapse longer than `D delete` (rank 70).
    // There is no `Enter activate` here — switching the active provider is the Models
    // picker's job; this surface only manages instances.
    let browse_hints: [FooterHint; 5] = [
        FooterHint::navigation(keyvocab::ARROWS_UD, "navigate"),
        FooterHint::secondary("/", "search"),
        FooterHint::primary("a", "add"),
        FooterHint::secondary("e", "edit"),
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
            &format!(
                "Connections{}keybindings",
                crate::tui::design::JOIN_BREADCRUMB
            ),
            theme,
        );
        let body = keymap_body_lines(hints, extra, theme);
        render_body(
            frame,
            f.body,
            body,
            scroll,
            None,
            SCROLL_EDGE_MARGIN,
            false,
            theme,
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

    // Empty state: no provider instance exists. Show a centered hint that
    // points the user at the `a` footer shortcut to add one (browse mode only
    // — in search mode the standard "no matches" body applies).
    if providers.is_empty() && !search {
        let body = connections_empty_body(theme);
        render_body(
            frame,
            body_rect,
            body,
            scroll,
            None,
            SCROLL_EDGE_MARGIN,
            false,
            theme,
        );
        if let Some(fo) = f.footer {
            render_modal_footer_with_more(frame, fo, hints, extra, theme);
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
/// name, then a dim provider suffix. Enter activates the highlighted pair; `*`
/// favorites the model (favorite is model-level, ADR-0046); `e` opens its
/// per-model settings (effort/thinking). There is **no delete** here — models
/// are served by their provider, so they cannot be removed from this surface.
/// Same browse/search two-mode design as the Connections modal.
///
/// `models` is the pre-computed flat row set; `modal_index` selects into it.
/// `scroll` is read and written back so the offset stays consistent with the
/// clamped body height; `follow_selection` keeps `modal_index` in view after
/// navigation.
#[allow(clippy::too_many_arguments)]
pub fn draw_models_modal(
    frame: &mut Frame,
    _layout_map: &mut LayoutMap,
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
) -> neenee_tui_engine::Rect {
    let area = modal_area(frame, FixedModalSpec::PROVIDER);
    let f = modal_frame(frame, area, theme.panel(), true, true);

    let header_rect = f.header;

    // No destructive action here — models are served by their provider and
    // cannot be removed from this surface. Favorite is model-level (ADR-0046).
    let browse_hints: [FooterHint; 6] = [
        FooterHint::navigation(keyvocab::ARROWS_UD, "navigate"),
        FooterHint::secondary("/", "search"),
        FooterHint::primary(keyvocab::ENTER, "activate"),
        FooterHint::secondary("*", "favorite"),
        FooterHint::secondary("e", "settings"),
        FooterHint::always(keyvocab::ESC, "close"),
    ];
    let search_hints: [FooterHint; 4] = [
        FooterHint::secondary("type", "filter"),
        FooterHint::navigation(keyvocab::ARROWS_UD, "navigate"),
        FooterHint::primary(keyvocab::ENTER, "activate"),
        FooterHint::always(keyvocab::ESC, "clear search"),
    ];
    let (hints, extra): (&[FooterHint], &[FooterHintWithBand]) = if search {
        (&search_hints, &[])
    } else {
        (&browse_hints, &[])
    };

    if keymap_open {
        // Breadcrumb: `Models` modal › its keybindings sub-page.
        modal_header(
            frame,
            header_rect,
            &format!("Models{}keybindings", crate::tui::design::JOIN_BREADCRUMB),
            theme,
        );
        let body = keymap_body_lines(hints, extra, theme);
        render_body(
            frame,
            f.body,
            body,
            scroll,
            None,
            SCROLL_EDGE_MARGIN,
            false,
            theme,
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

    // Flat model rows map 1:1 to `modal_index`.
    let body = model_list_body(
        models,
        current_provider,
        current_model,
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

/// Build the **Connections** provider list body via the shared [`ListRow`]
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
    use crate::tui::components::options::{ChoiceTone, choice_style};
    use crate::tui::components::row::{GUTTER, ListRow, RowGroup, RowStyledAtom};

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
        if let Some(label) = crate::tui::providers::provider_type_label(&rp.template_id) {
            row = row.group(RowGroup::midpoint().text(label, style.dim, 0));
        }

        body.push(row.finish());
    }
    body
}

/// The Connections empty-state body: shown when no provider instance exists.
/// A centered hint that points the user at the `a` footer shortcut to add one.
fn connections_empty_body(theme: &Theme) -> Vec<Line<'static>> {
    vec![
        Line::from(""),
        Line::from(""),
        Line::from(Span::styled(
            " No connections yet",
            Style::default().fg(theme.fg()).add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(
            " Press a to add a provider connection",
            Style::default().fg(theme.muted()),
        )),
    ]
}

/// Build the **Models** flat model list body via the shared [`ListRow`]
/// standard. Each row is a two-column layout spread across the width:
/// - a status group (fixed): the `●` current-state dot and the `★` favorite
///   star;
/// - column 1 (fixed): the model name (bold, fuzzy-highlighted in search);
/// - column 2 (midpoint): the provider label (dim), anchored at the horizontal
///   center so identical model ids served by different instances stay cleanly
///   separated as a second column — no `·`;
/// - an optional trailing reasoning tag (`◆ think on`), right-pinned.
///
/// The row fills the full `body_width` edge-to-edge. Favorite is model-level
/// (ADR-0046).
fn model_list_body(
    models: &[RankedModel],
    current_provider: &str,
    current_model: &str,
    modal_index: usize,
    theme: &Theme,
    body_width: usize,
) -> Vec<Line<'static>> {
    use crate::tui::components::options::{ChoiceTone, choice_style};
    use crate::tui::components::row::{GROUP_GAP, GUTTER, ListRow, RowGroup, RowStyledAtom};

    if models.is_empty() {
        return empty_body(theme);
    }
    let mut body: Vec<Line> = Vec::new();
    for (row, rm) in models.iter().enumerate() {
        let is_current = rm.provider_id == current_provider && rm.model == current_model;
        let is_selected = row == modal_index;
        let style = choice_style(ChoiceTone::Filled, is_selected, theme);

        // Status group (fixed): the two independent state glyphs. The
        // current-state dot borrows the `ok` tone (green = active); the
        // favorite star borrows `warn` when set, else stays muted/blank.
        let status = RowGroup::fixed()
            .glyph(
                if is_current { "●" } else { " " },
                if is_current { theme.ok() } else { style.dim },
                0,
            )
            .glyph(
                if rm.favorite { "★" } else { " " },
                if rm.favorite { theme.warn() } else { style.dim },
                1,
            );

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

        // Column 1 (model name) is capped to the left half so it never runs
        // into the midpoint provider column. Reserve the status group width,
        // its gutter + following GROUP_GAP, and the trailing tag if any.
        let status_w = 4; // dot + gap + star
        let tag_w = if tag.is_empty() { 0 } else { tag.width() + 2 }; // glyph + gap
        let name_budget = (body_width / 2)
            .saturating_sub(GUTTER + status_w + GROUP_GAP)
            .saturating_sub(tag_w)
            .max(1);
        let name = truncate_ellipsis(&rm.label, name_budget);

        // Column 1: the model name, one styled atom per char so fuzzy matches
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

        // Column 2 (midpoint): the provider label, anchored at the horizontal
        // center so the two columns spread across the width.
        let mut list_row = ListRow::new(style, body_width)
            .group(status)
            .group(identity)
            .group(RowGroup::midpoint().text(rm.provider_label.as_str(), style.dim, 0));

        // Optional trailing reasoning tag, right-pinned and info-toned. On a
        // brand-filled selected row it lifts to the contrast foreground.
        if !tag.is_empty() {
            let tag_fg = if is_selected {
                list_row.fill_fg()
            } else {
                theme.info()
            };
            list_row = list_row.group(
                RowGroup::trailing()
                    .glyph("◆", tag_fg, 0)
                    .text(tag, tag_fg, 1),
            );
        }
        body.push(list_row.finish());
    }
    body
}

/// The "no matches" placeholder body shared by both pickers.
fn empty_body(theme: &Theme) -> Vec<Line<'static>> {
    vec![
        Line::from(""),
        Line::from(Span::styled(
            " (no matches — try a shorter or different query)",
            Style::default().fg(theme.muted()),
        )),
    ]
}

/// Char indices the fuzzy match highlights, as a set for O(1) per-char lookup.
fn match_set(m: Option<&crate::tui::fuzzy::FuzzyMatch>) -> std::collections::HashSet<usize> {
    m.map(|m| m.positions.iter().copied().collect())
        .unwrap_or_default()
}

// ── Effort selector (segmented flat ladder) ─────────────────────────────────

/// The leading label column shared by the editor's rows (`" {:<8}"`).
const EFFORT_LABEL_W: usize = 9;

/// Resolve a wire effort string to its typed tier, for the caption text. An
/// unrecognized value yields `None` (no caption rather than a wrong one).
fn effort_tier(wire: &str) -> Option<neenee_core::effort::Effort> {
    neenee_core::effort::Effort::parse(wire)
}

/// The current tier's caption, e.g. `deep reasoning — the default for real
/// work`. Empty when the wire value is unrecognized.
fn effort_caption(wire: &str) -> &'static str {
    effort_tier(wire).map(|e| e.description()).unwrap_or("")
}

/// The flat selector's own width: label + every tier (the selected one
/// bracketed), tiers joined by two-space gaps. This is the *only* thing that
/// decides flat-vs-carousel — the caption is a trailing nicety that yields
/// (truncates, then drops) to whatever width the selector leaves behind, so a
/// long caption never forces the whole ladder into the carousel form.
fn flat_selector_width(levels: &[String], selected: &str) -> usize {
    let tiers_w: usize = levels
        .iter()
        .map(|l| {
            UnicodeWidthStr::width(l.as_str()) + usize::from(l == selected) * 2 // [ ] wraps
        })
        .sum::<usize>()
        + 2 * levels.len().saturating_sub(1); // inter-tier gaps
    EFFORT_LABEL_W + tiers_w
}

/// True when the segmented ladder itself fits on one body row. When the ladder
/// is unknown (empty `levels`) we cannot lay it out flat at all; when it fits,
/// the caption rides along in whatever space remains.
fn effort_block_flat(body_width: usize, levels: &[String]) -> bool {
    if levels.is_empty() {
        return false;
    }
    // Use the widest possible selection (the longest tier name gets the
    // brackets) so the flat decision is stable as the selection moves — the
    // row must never flip between flat and carousel as the user cycles.
    let widest = levels
        .iter()
        .map(|l| flat_selector_width(levels, l))
        .max()
        .unwrap_or(0);
    widest <= body_width
}

/// Build the segmented-ladder line(s) for the effort selector. `flat` selects
/// the one-row form (selector + trailing caption); the wrapped form emits a
/// selector row and a separate caption row. `focused` highlights the selected
/// tier in the brand tone (and, for `max`, the ignition accent).
fn effort_block_lines(
    current: &str,
    levels: &[String],
    flat: bool,
    body_width: usize,
    focused: bool,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let label = format!(" {:<8}", "Effort");
    let caption = effort_caption(current);
    let selected_fg = if focused { theme.brand() } else { theme.fg() };
    // Width the caption may occupy after the selector on a flat row, or the
    // full body width on its own wrapped row. Zero hides the caption entirely.
    let caption_budget = if flat {
        body_width.saturating_sub(flat_selector_width(levels, current) + 3)
    } else {
        body_width.saturating_sub(EFFORT_LABEL_W)
    };
    let caption = truncate_ellipsis(caption, caption_budget);

    // Compact carousel fallback: unknown ladder, or too narrow to lay flat.
    // `< current >` with a position pip for each rung so the depth still reads.
    if levels.is_empty() || !flat {
        let value_style = Style::default()
            .fg(if focused { theme.brand() } else { theme.fg() })
            .add_modifier(Modifier::BOLD);
        let chev_style = Style::default().fg(theme.muted());
        let mut spans = vec![
            Span::styled(label, Style::default().fg(theme.brand()).add_modifier(Modifier::BOLD)),
            Span::raw(" ".to_string()),
            Span::styled("< ".to_string(), chev_style),
            Span::styled(current.to_string(), value_style),
            Span::styled(" >".to_string(), chev_style),
        ];
        // Position pips: one per rung, the current one solid — the depth
        // direction stays visible even in the carousel form.
        if !levels.is_empty() {
            let cur_idx = levels.iter().position(|l| l == current);
            let pips: String = levels
                .iter()
                .enumerate()
                .map(|(i, _)| if Some(i) == cur_idx { '●' } else { '○' })
                .collect::<Vec<char>>()
                .into_iter()
                .collect();
            spans.push(Span::raw("  ".to_string()));
            spans.push(Span::styled(pips, Style::default().fg(theme.muted())));
        }
        let mut lines = vec![Line::from(spans)];
        if !caption.is_empty() {
            lines.push(Line::from(vec![
                Span::raw(" ".repeat(EFFORT_LABEL_W)),
                Span::styled(caption.to_string(), Style::default().fg(theme.muted())),
            ]));
        }
        return lines;
    }

    // Flat segmented ladder: every tier laid out left-to-right in ascending
    // depth, the selected one bracketed and bolded. `max` additionally takes
    // the warning tone so the top rung reads as the ignition tier it is
    // (mirrors the hint bar's `M A X` celebration).
    let mut spans = vec![Span::styled(
        label,
        Style::default().fg(theme.brand()).add_modifier(Modifier::BOLD),
    )];
    for (i, level) in levels.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw("  ".to_string()));
        }
        let is_sel = level == current;
        if is_sel {
            let fg = if level == "max" {
                theme.warn()
            } else {
                selected_fg
            };
            spans.push(Span::styled(
                "[".to_string(),
                Style::default().fg(theme.muted()),
            ));
            spans.push(Span::styled(
                level.clone(),
                Style::default().fg(fg).add_modifier(Modifier::BOLD),
            ));
            spans.push(Span::styled(
                "]".to_string(),
                Style::default().fg(theme.muted()),
            ));
        } else {
            spans.push(Span::styled(
                level.clone(),
                Style::default().fg(theme.muted()),
            ));
        }
    }
    // Trailing caption on the same row, after a gap.
    if !caption.is_empty() {
        spans.push(Span::raw("   ".to_string()));
        spans.push(Span::styled(
            caption.to_string(),
            Style::default().fg(theme.muted()),
        ));
    }
    vec![Line::from(spans)]
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
    // laid out flat. `None` effort hides the whole block.
    effort: Option<&str>,
    // The model's advertised effort ladder (wire strings, ascending). Drives
    // the segmented flat layout; may be empty when the caller could not
    // resolve the model, in which case the selector degrades to the compact
    // carousel form.
    effort_levels: &[String],
    // `thinking`: when `Some`, render an extended-thinking on/off row showing
    // a checkbox; toggled with Space by the caller.
    // `None` hides. Orthogonal to effort.
    thinking: Option<bool>,
    theme: &Theme,
) -> neenee_tui_engine::Rect {
    let geometry = ContentModalSpec::MODEL_EDITOR;

    // Content-driven height: the editor's row count is width-independent, so
    // size the panel to exactly fit the rows rather than reserving a fixed 30%
    // slab that left most of the panel empty. The effort block occupies one
    // row when it fits flat (the segmented ladder plus a caption that shares
    // the row) and two rows when it must wrap (selector row + caption row), so
    // we probe the body width before counting. We add the modal chrome and let
    // `content_modal_area` clamp the total to a sane viewport fraction.
    // The probe area shares the final width, so subtract the symmetric inner
    // padding `modal_frame` applies to recover the exact body width.
    let body_width = content_modal_probe(frame, geometry)
        .width
        .saturating_sub(2 * crate::tui::design::MODAL_INNER_H_PADDING) as usize;
    let effort_rows = match effort {
        Some(_) if effort_block_flat(body_width, effort_levels) => 1,
        Some(_) => 2, // wrapped: selector row + caption row
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
        let label = format!(" {:<8}", "API key");
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

    // Effort block (optional): the segmented flat ladder when the body is wide
    // enough, otherwise the compact carousel. Either way the current tier's
    // caption rides along — flat shares the selector's row, wrapped drops it
    // onto its own row beneath. The value is cycled with ←/→ or jumped to with
    // a digit (not typed), so the live text is the current selection.
    if let Some(effort) = effort {
        let flat = effort_block_flat(body_width, effort_levels);
        for line in
            effort_block_lines(effort, effort_levels, flat, body_width, focused_field == 1, theme)
        {
            body.push(line);
        }
    }

    // Thinking block (optional): a real on/off checkbox, not a carousel — the
    // boolean reads as `[x]`/`[ ]` and toggles with Space (a non-text field,
    // so no caret while focused). Orthogonal to effort.
    if let Some(on) = thinking {
        let label = format!(" {:<8}", "Thinking");
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

    // Place the caret on the focused row, after its label. The effort selector
    // is a cycled / jumped value (no editable caret position), so when it's
    // focused we park the cursor just past its label. The thinking row is a
    // toggle (no text), so we hide the caret entirely while it's focused.
    if focused_field != 2 {
        let prefix = if focused_field == 1 {
            format!(" {:<8}", "Effort")
        } else {
            format!(" {:<8}", "API key")
        };
        let row_offset = if focused_field == 1 && show_key { 1 } else { 0 };
        // Subtract the focused field's viewport offset so the caret tracks the
        // visible (scrolled) text, and clamp it to stay inside the body rect.
        let caret_col = caret_column(input, cursor_position);
        let off = if focused_field == 0 {
            api_key_off as u16
        } else {
            0
        };
        let max_x = body_rect.x + body_rect.width.saturating_sub(1);
        let mut cursor_x = (body_rect.x + prefix.width() as u16 + caret_col).saturating_sub(off);
        if cursor_x > max_x {
            cursor_x = max_x;
        }
        let cursor_y = body_rect.y + row_offset as u16;
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
            let s_style = crate::tui::components::options::choice_style(
                crate::tui::components::options::ChoiceTone::Flat,
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
    frame: &mut Frame,
    theme: &Theme,
    scroll: &mut usize,
) -> neenee_tui_engine::Rect {
    let area = modal_area(frame, FixedModalSpec::OAUTH_PENDING);
    let f = modal_frame(frame, area, theme.panel(), true, true);

    if let Some(h) = f.header {
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    title.to_string(),
                    Style::default()
                        .fg(theme.brand())
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(" · authorizing…", Style::default().fg(theme.muted())),
            ])),
            h,
        );
    }

    let mut body: Vec<Line> = Vec::new();
    if let Some(err) = error {
        body.push(Line::from(Span::styled(
            format!("✗ {err}"),
            Style::default().fg(theme.err()),
        )));
        body.push(Line::from(""));
        body.push(Line::from(Span::styled(
            "Esc to go back and try again.",
            Style::default().fg(theme.muted()),
        )));
    } else {
        if !message.is_empty() {
            body.push(Line::from(Span::styled(
                message.to_string(),
                Style::default().fg(theme.fg()),
            )));
            body.push(Line::from(""));
        }
        if !url.is_empty() {
            body.push(Line::from(Span::styled(
                "Open this link if the browser did not open:",
                Style::default().fg(theme.muted()),
            )));
            body.push(Line::from(Span::styled(
                url.to_string(),
                Style::default()
                    .fg(theme.brand())
                    .add_modifier(Modifier::UNDERLINED),
            )));
            body.push(Line::from(""));
        } else {
            body.push(Line::from(Span::styled(
                "Starting authorization…",
                Style::default().fg(theme.muted()),
            )));
            body.push(Line::from(""));
        }
        if !user_code.is_empty() {
            body.push(Line::from(vec![
                Span::styled("Code: ", Style::default().fg(theme.muted())),
                Span::styled(
                    user_code.to_string(),
                    Style::default().fg(theme.fg()).add_modifier(Modifier::BOLD),
                ),
            ]));
            body.push(Line::from(""));
        }
        body.push(Line::from(Span::styled(
            "Waiting for authorization…",
            Style::default().fg(theme.muted()),
        )));
    }

    render_body(
        frame,
        f.body,
        body,
        scroll,
        None,
        SCROLL_EDGE_MARGIN,
        true,
        theme,
    );

    if let Some(fo) = f.footer {
        // The copy affordances are only useful when the relevant field is
        // populated: the device code once the device-code request has returned,
        // and the URL alongside it. On the error branch neither is offered, so
        // only the cancel hint shows.
        let mut hints: Vec<FooterHint> = Vec::new();
        if error.is_none() {
            if !user_code.is_empty() {
                hints.push(FooterHint::secondary("c", "copy code"));
            }
            if !url.is_empty() {
                hints.push(FooterHint::secondary("u", "copy url"));
            }
        }
        hints.push(FooterHint::always(keyvocab::ESC, "cancel"));
        render_modal_footer(frame, fo, &hints, theme);
    }
    area
}

/// Draw the provider-template chooser as the Connections list's Add connection
/// child page. It retains the parent panel geometry and uses a breadcrumb
/// header so navigation does not look like a separate modal. Each row is a
/// label + a muted one-line description; `↑/↓` move the highlight and Enter
/// opens the editor.
/// `scroll` is read AND written back so the offset stays consistent with the
/// clamped body height; the highlighted template is followed on-screen so
/// `↑/↓` navigation keeps it visible even when the list overflows the body.
pub fn draw_provider_template_chooser(
    selected: usize,
    frame: &mut Frame,
    theme: &Theme,
    scroll: &mut usize,
) -> neenee_tui_engine::Rect {
    let area = modal_area(frame, FixedModalSpec::PROVIDER);
    let f = modal_frame(frame, area, theme.panel(), true, true);

    let header = breadcrumb_parts("Connections", "Add connection");
    modal_header_parts(frame, f.header, &header, theme);

    let mut body: Vec<Line> = Vec::new();
    for (i, template) in PROVIDER_TEMPLATES.iter().enumerate() {
        let s_style = crate::tui::components::options::choice_style(
            crate::tui::components::options::ChoiceTone::Flat,
            i == selected,
            theme,
        );
        let (marker, label_style) = if i == selected {
            (
                " › ",
                Style::default().fg(s_style.fg).add_modifier(Modifier::BOLD),
            )
        } else {
            ("   ", Style::default().fg(s_style.fg))
        };
        let model_count = template.models.len();
        let model_meta = if model_count == 1 {
            "1 model".to_string()
        } else {
            format!("{model_count} models")
        };
        body.push(Line::from(vec![
            Span::styled(format!("{marker}{}", template.label), label_style),
            Span::styled(
                format!(" · {} · {model_meta}", template.protocol),
                Style::default().fg(s_style.dim),
            ),
        ]));
        body.push(Line::from(Span::styled(
            format!("     {}", template.description),
            Style::default().fg(s_style.dim),
        )));
    }

    // Each template is a 2-line block (label + description, no blank gap) so
    // the chooser stays compact; the highlighted block starts at
    // `selected * 2`. Following that visual line keeps the highlighted entry
    // in view as `↑/↓` wraps around.
    let follow = selected.checked_mul(2);
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
) -> neenee_tui_engine::Rect {
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

    let area = modal_area(frame, FixedModalSpec::CUSTOM_PROVIDER);
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

    let header = breadcrumb_parts("Connections", title);
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
    fn buffer_text(terminal: &neenee_tui_engine::TestTerminal) -> String {
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
        let mut terminal = neenee_tui_engine::TestTerminal::new(width, height);
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
    fn effort_selector_lays_the_ladder_flat_when_wide() {
        // Wide enough: every tier is spelled out on one row, ascending, with
        // the selected one bracketed — the whole ladder is visible at a glance.
        let full = levels(&["low", "medium", "high", "xhigh", "max"]);
        let text = render_settings_editor(120, 24, Some("high"), &full, None);
        let effort_row = text
            .lines()
            .find(|l| l.contains("Effort"))
            .expect("effort row");
        for tier in ["low", "medium", "high", "xhigh", "max"] {
            assert!(effort_row.contains(tier), "missing tier: {effort_row:?}");
        }
        assert!(
            effort_row.contains("[high]"),
            "selected tier bracketed: {effort_row:?}"
        );
        // Depth order is left-to-right ascending.
        let low = effort_row.find("low").unwrap();
        let max = effort_row.find("max").unwrap();
        assert!(low < max, "ladder must ascend left→right: {effort_row:?}");
        // The carousel affordance is gone in the flat form.
        assert!(!effort_row.contains('<'), "no carousel chevrons: {effort_row:?}");
    }

    #[test]
    fn effort_selector_wraps_to_carousel_when_narrow() {
        // Too narrow for the ladder: the compact `< current >` carousel shows,
        // with one position pip per rung and the current rung solid so the
        // depth direction still reads.
        let full = levels(&["low", "medium", "high", "xhigh", "max"]);
        let text = render_settings_editor(56, 24, Some("high"), &full, None);
        let effort_row = text
            .lines()
            .find(|l| l.contains("Effort"))
            .expect("effort row");
        assert!(effort_row.contains("< high >"), "carousel: {effort_row:?}");
        assert!(
            effort_row.contains("○○●○○"),
            "position pips, current solid: {effort_row:?}"
        );
    }

    #[test]
    fn effort_caption_rides_along_in_both_forms() {
        // The current tier's caption shows under the selector — truncated to
        // the available width rather than dropped or wrapped awkwardly.
        let full = levels(&["low", "medium", "high", "xhigh", "max"]);
        let wide = render_settings_editor(120, 24, Some("high"), &full, None);
        assert!(
            wide.contains("deep reasoning"),
            "flat caption present: {wide:?}"
        );
        let narrow = render_settings_editor(56, 24, Some("high"), &full, None);
        assert!(
            narrow.contains("deep reasoning"),
            "carousel caption present: {narrow:?}"
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
    fn effort_block_flat_uses_widest_selection_for_stability() {
        // The flat decision must not flip as the user cycles: it is computed
        // for the widest bracketed selection. A 3-tier ladder fits a modest
        // body; a 6-tier ladder needs more room.
        let common = levels(&["low", "medium", "high"]);
        assert!(effort_block_flat(40, &common), "3-tier fits 40");
        let openai = levels(&["none", "minimal", "low", "medium", "high", "xhigh"]);
        assert!(!effort_block_flat(40, &openai), "6-tier overflows 40");
        // An unknown ladder can never lay flat.
        assert!(!effort_block_flat(80, &[]), "empty ladder → carousel");
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
}
