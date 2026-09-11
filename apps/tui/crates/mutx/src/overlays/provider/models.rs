//! The flat Models picker modal (selecting the active model/instance).

use mutx_engine::{
    Frame, {Line, Span}, {Modifier, Style},
};

use unicode_width::UnicodeWidthStr;

use super::super::common::truncate_ellipsis;
use super::common::{
    draw_picker_search_row, match_set, place_picker_search_cursor, search_empty_body,
    split_search_body,
};
use crate::components::options::{ChoiceTone, choice_style};
use crate::components::row::{GUTTER, ListRow, RowGroup, RowStyledAtom};
use crate::primitives::{
    BodyRenderOptions, FixedModalSpec, FooterHint, FooterHintWithBand, SCROLL_EDGE_MARGIN,
    keyvocab, modal_area, modal_frame, modal_header, render_body, render_centered_body,
    render_modal_footer_with_extra,
};
use crate::providers::{ModelBodyLine, RankedModel, models_body_lines};
use crate::render::Theme;

/// The narrowest leftover space worth spending on the wire-id suffix that
/// rides behind a provider-published name label. The suffix fills only what the
/// label leaves unused in the identity column, so below this there is nothing
/// legible to draw and the row shows the label alone — the mirror of a row whose
/// provider published no label at all. The floor is deliberately above a bare
/// fragment: `deep…` would read as a different model rather than as an
/// abbreviation of `deepseek-flash`.
const MIN_SUFFIX_BUDGET: usize = 10;

/// Properties for rendering the Models modal.
pub struct ModelsModalProps<'a> {
    pub models: &'a [RankedModel],
    pub current_provider: &'a str,
    pub current_model: &'a str,
    pub modal_index: usize,
    pub query: &'a str,
    pub cursor_position: usize,
    pub scroll: &'a mut usize,
    pub follow_selection: bool,
    pub search: bool,
    /// Whether this surface currently owns the terminal cursor
    /// (`App::caret_owner() == CaretOwner::Overlay`, ADR-0205). The picker
    /// never places the physical cursor on its own authority — the frame-level
    /// arbiter decides that, and this flag is its verdict threaded down.
    pub show_caret: bool,
    pub refreshing: bool,
    pub spinner_phase: usize,
}

/// Draw the **Models** flat model picker modal (`/models`).
pub fn draw_models_modal(
    frame: &mut Frame,
    props: ModelsModalProps<'_>,
    theme: &Theme,
) -> mutx_engine::Rect {
    let ModelsModalProps {
        models,
        current_provider,
        current_model,
        modal_index,
        query,
        cursor_position,
        scroll,
        follow_selection,
        search,
        show_caret,
        refreshing,
        spinner_phase,
    } = props;
    let area = modal_area(frame, FixedModalSpec::PROVIDER);
    let f = modal_frame(frame, area, theme, true, true);

    let header_rect = f.header;

    let refresh_label = if refreshing {
        "refreshing…"
    } else {
        "refresh"
    };
    let browse_hints: [FooterHint; 8] = [
        FooterHint::navigation(keyvocab::ARROWS_UD, "navigate"),
        FooterHint::secondary("/", "search"),
        FooterHint::key_primary(crate::keymap::Key::ENTER, "activate"),
        FooterHint::secondary("*", "favorite"),
        FooterHint::secondary("x", "block"),
        FooterHint::secondary("e", "settings"),
        FooterHint::secondary("r", refresh_label),
        FooterHint::key_always(crate::keymap::Key::ESC, "close"),
    ];
    let search_hints: [FooterHint; 4] = [
        FooterHint::secondary("type", "filter"),
        FooterHint::navigation(keyvocab::ARROWS_UD, "navigate"),
        FooterHint::key_primary(crate::keymap::Key::ENTER, "activate"),
        FooterHint::key_always(crate::keymap::Key::ESC, "clear search"),
    ];
    let empty_hints: [FooterHint; 3] = [
        FooterHint::primary("a", "add connection"),
        FooterHint::secondary("r", refresh_label),
        FooterHint::key_always(crate::keymap::Key::ESC, "close"),
    ];
    let (hints, extra): (&[FooterHint], &[FooterHintWithBand]) = if search {
        (&search_hints, &[])
    } else if models.is_empty() {
        (&empty_hints, &[])
    } else {
        (&browse_hints, &[])
    };

    if refreshing {
        let spin = theme.glyphs.spinner_frame(spinner_phase);
        let header = [
            crate::elevation::HeaderPart::title("Models"),
            crate::elevation::HeaderPart::Text {
                text: "  ",
                accent: false,
            },
            crate::elevation::HeaderPart::Text {
                text: spin,
                accent: true,
            },
            crate::elevation::HeaderPart::Text {
                text: " refreshing…",
                accent: false,
            },
        ];
        crate::elevation::modal_header_parts(frame, header_rect, &header, theme);
    } else {
        modal_header(frame, header_rect, "Models", theme);
    }

    let (search_rect, body_rect) = split_search_body(f.body, search);
    if let Some(search_rect) = search_rect {
        draw_picker_search_row(frame, search_rect, query, cursor_position, theme);
    }

    if models.is_empty() && !search {
        let body = if refreshing {
            let spin = theme.glyphs.spinner_frame(spinner_phase);
            vec![
                Line::from(""),
                Line::from(vec![
                    Span::styled(format!("{spin} "), Style::default().fg(theme.primary)),
                    Span::styled("Refreshing models…", Style::default().fg(theme.muted())),
                ]),
            ]
        } else {
            models_empty_body(theme)
        };
        render_centered_body(frame, body_rect, body);
        if let Some(fo) = f.footer {
            render_modal_footer_with_extra(frame, fo, hints, extra, theme);
        }
        return area;
    }

    if models.is_empty() && search {
        let body = search_empty_body(theme);
        render_centered_body(frame, body_rect, body);
        if let Some(fo) = f.footer {
            render_modal_footer_with_extra(frame, fo, hints, extra, theme);
        }
        if show_caret && let Some(sr) = search_rect {
            place_picker_search_cursor(frame, sr, query, cursor_position);
        }
        return area;
    }

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
        BodyRenderOptions::new(follow, SCROLL_EDGE_MARGIN, false),
        theme,
    );

    if let Some(fo) = f.footer {
        render_modal_footer_with_extra(frame, fo, hints, extra, theme);
    }

    if show_caret
        && search
        && let Some(sr) = search_rect
    {
        place_picker_search_cursor(frame, sr, query, cursor_position);
    }
    area
}

/// Build the **Models** flat model list body via the shared [`crate::components::row::ListRow`]
/// standard, **sectioned into three labeled groups** — Favorites, Recent, All models.
pub(crate) fn model_list_body(
    models: &[RankedModel],
    _current_provider: &str,
    _current_model: &str,
    modal_index: usize,
    theme: &Theme,
    body_width: usize,
) -> (Vec<Line<'static>>, Vec<usize>) {
    if models.is_empty() {
        return (Vec::new(), Vec::new());
    }
    let (geometry, row_line) = models_body_lines(models);
    let mut body: Vec<Line> = Vec::with_capacity(geometry.len() + 3);

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
                body.push(Line::from(Span::styled(
                    format!("{}{}", " ".repeat(GUTTER), section.label()),
                    Style::default().fg(theme.muted()),
                )));
            }
            ModelBodyLine::Row(row) => {
                let rm = &models[row];
                let is_selected = row == modal_index;
                let style = choice_style(ChoiceTone::Filled, is_selected, theme);

                let tag = match (rm.thinking, rm.effort.as_deref()) {
                    (Some(true), Some(effort)) => format!("think on {effort}"),
                    (Some(true), None) => "think on".to_string(),
                    (None, Some(effort)) => effort.to_string(),
                    _ => String::new(),
                };

                // The identity column is `body_width * 3 / 5` wide (the ratio
                // group below starts there). The rendered label — the
                // provider's own name for the model when it publishes one, else
                // the wire id — owns that column, and is what the fuzzy
                // highlight indexes. When the label is a name, the wire id
                // still rides along as a dim suffix in whatever padding the
                // label leaves unused: the id is the string that actually goes
                // on the wire and into config, so a name-first list must not
                // hide the value the user has to type. The suffix only fills
                // blank padding, so it never shortens the label, never pushes
                // the provider column out of alignment, and never widens the
                // row; no room means no suffix.
                let identity_budget = ((body_width * 3) / 5).saturating_sub(GUTTER + 1).max(1);
                let display = truncate_ellipsis(
                    rm.name.as_deref().unwrap_or(rm.model.as_str()),
                    identity_budget,
                );
                let id_suffix = rm.name.as_deref().and_then(|_| {
                    let budget = identity_budget.saturating_sub(display.as_str().width() + 1);
                    (budget >= MIN_SUFFIX_BUDGET).then(|| truncate_ellipsis(&rm.model, budget))
                });

                let matched = match_set(rm.m.as_ref());
                let mut identity = RowGroup::fixed();
                for (char_idx, c) in display.chars().enumerate() {
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
                // The wire id behind a name label, dimmed and set one column off
                // the label so the pair reads as ONE identity column: the label
                // is what the provider calls it, the id is what you type. It
                // stays inside the identity group (never the provider column)
                // so tabular alignment with label-less rows is preserved.
                if let Some(id_suffix) = id_suffix {
                    identity = identity.styled(
                        RowStyledAtom {
                            text: id_suffix,
                            style: Style::default().bg(style.bg).fg(style.dim),
                        },
                        1,
                    );
                }

                let mut list_row = ListRow::new(style, body_width)
                    .group(identity)
                    .group(RowGroup::ratio(3, 5).text(rm.provider_label.as_str(), style.dim, 0));

                if !tag.is_empty() {
                    let tag_fg = if is_selected {
                        list_row.fill_fg()
                    } else {
                        theme.info()
                    };
                    list_row = list_row.group(RowGroup::trailing().text(tag, tag_fg, 0));
                }
                body.push(list_row.finish());
            }
        }
    }
    (body, row_line)
}

/// The Models empty-state body: shown when no model exists (no provider configured
/// or no models returned).
pub(crate) fn models_empty_body(theme: &Theme) -> Vec<Line<'static>> {
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
