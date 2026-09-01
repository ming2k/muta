//! The flat Models picker modal (selecting the active model/instance).

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
    keymap_body_lines, keymap_page_footer_hints, keyvocab, modal_area, modal_frame, modal_header,
    render_body, render_centered_body, render_modal_footer_with_more,
};
use crate::providers::{ModelBodyLine, RankedModel, models_body_lines};
use crate::view::Theme;

/// Draw the **Models** flat model picker modal (`/models`). A single searchable
/// list of every model declared across every provider instance (built-in + user
/// custom). Selecting a row activates that model ON that connection.
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

    let browse_hints: [FooterHint; 7] = [
        FooterHint::navigation(keyvocab::ARROWS_UD, "navigate"),
        FooterHint::secondary("/", "search"),
        FooterHint::key_primary(crate::keymap::Key::ENTER, "activate"),
        FooterHint::secondary("*", "favorite"),
        FooterHint::secondary("e", "settings"),
        FooterHint::secondary("r", "refresh"),
        FooterHint::key_always(crate::keymap::Key::ESC, "close"),
    ];
    let search_hints: [FooterHint; 4] = [
        FooterHint::secondary("type", "filter"),
        FooterHint::navigation(keyvocab::ARROWS_UD, "navigate"),
        FooterHint::key_primary(crate::keymap::Key::ENTER, "activate"),
        FooterHint::key_always(crate::keymap::Key::ESC, "clear search"),
    ];
    let empty_hints: [FooterHint; 2] = [
        FooterHint::primary("a", "add connection"),
        FooterHint::key_always(crate::keymap::Key::ESC, "close"),
    ];
    let (hints, extra): (&[FooterHint], &[FooterHintWithBand]) = if search {
        (&search_hints, &[])
    } else if models.is_empty() {
        (&empty_hints, &[])
    } else {
        (&browse_hints, &[])
    };

    if keymap_open {
        modal_header(
            frame,
            header_rect,
            &format!("Models{}keybindings", crate::design::JOIN_BREADCRUMB),
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
            crate::primitives::render_modal_footer(frame, fo, &keymap_page_footer_hints(), theme);
        }
        return area;
    }

    modal_header(frame, header_rect, "Models", theme);

    let (search_rect, body_rect) = split_search_body(f.body, search);
    if let Some(search_rect) = search_rect {
        draw_picker_search_row(frame, search_rect, query, cursor_position, theme);
    }

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
        render_modal_footer_with_more(frame, fo, hints, extra, theme);
    }

    if search && let Some(sr) = search_rect {
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

                let name_budget = ((body_width * 3) / 5).saturating_sub(GUTTER + 1).max(1);
                let name = truncate_ellipsis(&rm.model, name_budget);

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
