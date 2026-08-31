//! Modular Settings View (`/settings`): first-class, full-screen configuration center (ADR-0141).
//!
//! Subdivided into dedicated per-category modules:
//! - [`appearance`]: Themes and palette swatches
//! - [`transcript`]: Message boundaries, Turn Band layout, auto-scroll
//! - [`behavior`]: Click-outside dismiss and mouse rules
//! - [`web`]: Search & Reader connection routing, proxy, timeout
//! - [`system`]: Paths, runtime info, version

pub mod appearance;
pub mod behavior;
pub mod system;
pub mod transcript;
pub mod web;

pub use web::{
    build_add_web_connection_dropdown, build_websearch_provider_dropdown,
    build_websearch_reader_dropdown,
};

use muta_contracts::ColorSchemeConfig;
use mutx_engine::{
    Block as RtBlock, Clear, Constraint, Direction, Frame, Layout, Line, Modifier, Paragraph, Rect,
    Span, Style, Wrap,
};
use unicode_width::UnicodeWidthStr;

use crate::primitives::{SCROLL_EDGE_MARGIN, draw_scrollbar, resolve_scroll};
use crate::view::Theme;
use crate::view_header::{
    SettingsHead, ViewHeader, ViewHints, ViewKind, draw_view_header, draw_view_header_hints,
};

/// Which pane of the Settings View currently owns keyboard focus.
#[derive(PartialEq, Eq, Clone, Copy, Debug, Default)]
pub enum ConfigFocus {
    /// Left pane: Category navigation (Appearance, Transcript, Behavior, Web & Search, System).
    #[default]
    Categories,
    /// Right pane: Detail configuration options and controls.
    Detail,
}

/// The top-level settings categories.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigCategory {
    Appearance = 0,
    Transcript = 1,
    Behavior = 2,
    WebSearch = 3,
    System = 4,
}

impl ConfigCategory {
    pub const ALL: [ConfigCategory; 5] = [
        ConfigCategory::Appearance,
        ConfigCategory::Transcript,
        ConfigCategory::Behavior,
        ConfigCategory::WebSearch,
        ConfigCategory::System,
    ];

    pub fn from_index(index: usize) -> Self {
        match index % Self::ALL.len() {
            0 => ConfigCategory::Appearance,
            1 => ConfigCategory::Transcript,
            2 => ConfigCategory::Behavior,
            3 => ConfigCategory::WebSearch,
            _ => ConfigCategory::System,
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            ConfigCategory::Appearance => "Appearance",
            ConfigCategory::Transcript => "Transcript",
            ConfigCategory::Behavior => "Behavior",
            ConfigCategory::WebSearch => "Web",
            ConfigCategory::System => "System & Info",
        }
    }

    pub fn subtitle(self) -> &'static str {
        match self {
            ConfigCategory::Appearance => "Themes & palette swatches",
            ConfigCategory::Transcript => "Turn bands, auto-scroll & disclosures",
            ConfigCategory::Behavior => "Click-outside dismiss & interaction rules",
            ConfigCategory::WebSearch => "Search & fetch connections, routing & proxy",
            ConfigCategory::System => "Config file paths, runtime & daemon info",
        }
    }
}

/// Geometry sub-rects returned by [`draw_settings_view`].
#[derive(Clone, Copy, Debug)]
#[allow(dead_code)]
pub struct ConfigRects {
    pub area: Rect,
    pub category_body: Rect,
    pub detail_body: Rect,
}

/// Properties passed to render the complete Settings View.
pub struct ConfigViewProps<'a> {
    pub category_index: usize,
    pub detail_index: usize,
    pub web_segment: usize,
    pub focus: ConfigFocus,
    pub color_scheme: &'a str,
    pub custom_color_scheme: &'a ColorSchemeConfig,
    pub transcript_layout: crate::view::layout::Strategy,
    pub expand_auto_scroll: bool,
    pub click_outside_dismiss: bool,
    pub websearch: Option<&'a muta_contracts::WebSearchConfigView>,
    pub workspace: &'a str,
    pub category_scroll: &'a mut usize,
    pub detail_scroll: &'a mut usize,
    pub breadcrumbs: Option<&'a str>,
    pub theme: &'a Theme,
}

/// Draw the full-screen Settings View.
pub fn draw_settings_view(frame: &mut Frame, mut props: ConfigViewProps<'_>) -> ConfigRects {
    let area = frame.area();
    frame.render_widget(Clear, area);

    // Fill full background with canvas tone
    frame.render_widget(
        RtBlock::default().style(Style::default().bg(props.theme.body())),
        area,
    );

    // 5 vertical zones: Top Header (1 row), Breadcrumbs Subhead (1 row), Gap (1 row), Center Body (flexible), Bottom Footer (3 rows).
    let vertical_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(8),
            Constraint::Length(3),
        ])
        .split(area);

    let header_rect = vertical_chunks[0];
    let subhead_rect = vertical_chunks[1];
    let body_rect = vertical_chunks[3];
    let footer_rect = vertical_chunks[4];

    let category = ConfigCategory::from_index(props.category_index);

    // 1. Top Header Row
    let header = ViewHeader::Settings(&SettingsHead {
        workspace: props.workspace,
        category: category.title(),
        subtitle: category.subtitle(),
    });
    draw_view_header(frame, header_rect, &header, props.theme);

    // 2. View Stack Breadcrumbs & Affordance
    let view_hints = ViewHints {
        kind: ViewKind::Settings,
        asides: None,
        interruptible: false,
        parent_note: "",
        breadcrumbs: props.breadcrumbs,
    };
    draw_view_header_hints(frame, subhead_rect, &view_hints, props.theme);

    // 3. Center Body (Dual-Pane Master-Detail Split with Contrasting Tones)
    let body_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(26), Constraint::Min(40)])
        .split(body_rect);

    let category_rect = body_chunks[0];
    let detail_rect = body_chunks[1];

    // Left pane contrasting surface
    frame.render_widget(
        RtBlock::default().style(Style::default().bg(props.theme.surface())),
        category_rect,
    );
    // Right pane main canvas body
    frame.render_widget(
        RtBlock::default().style(Style::default().bg(props.theme.body())),
        detail_rect,
    );

    draw_categories_pane(frame, category_rect, &mut props);

    let focused = props.focus == ConfigFocus::Detail;
    match category {
        ConfigCategory::Appearance => {
            appearance::draw_appearance_detail(frame, detail_rect, &mut props, focused)
        }
        ConfigCategory::Transcript => {
            transcript::draw_transcript_detail(frame, detail_rect, &mut props, focused)
        }
        ConfigCategory::Behavior => {
            behavior::draw_behavior_detail(frame, detail_rect, &mut props, focused)
        }
        ConfigCategory::WebSearch => {
            web::draw_websearch_detail(frame, detail_rect, &mut props, focused)
        }
        ConfigCategory::System => {
            system::draw_system_detail(frame, detail_rect, &mut props, focused)
        }
    }

    // 4. Bottom Footer (3-Row Runner-Style)
    draw_footer(frame, footer_rect, props.focus, props.theme);

    ConfigRects {
        area,
        category_body: category_rect,
        detail_body: detail_rect,
    }
}

fn draw_categories_pane(frame: &mut Frame, area: Rect, props: &mut ConfigViewProps<'_>) {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let is_focused = props.focus == ConfigFocus::Categories;

    let selected_line = Some(props.category_index * 2);

    for (i, cat) in ConfigCategory::ALL.iter().enumerate() {
        let is_selected = i == props.category_index;
        let cursor = if is_selected { "›" } else { " " };

        let style = if is_selected && is_focused {
            Style::default()
                .fg(props.theme.brand())
                .add_modifier(Modifier::BOLD)
        } else if is_selected {
            Style::default()
                .fg(props.theme.fg())
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(props.theme.muted())
        };

        let cursor_style = if is_selected {
            Style::default()
                .fg(props.theme.brand())
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(props.theme.dim())
        };

        lines.push(Line::from(vec![
            Span::styled(format!(" {cursor} "), cursor_style),
            Span::styled(cat.title(), style),
        ]));
        lines.push(Line::from(""));
    }

    let visible_rows = area.height as usize;
    let content_len = lines.len();

    let (content_offset, max_scroll) = resolve_scroll(
        props.category_scroll,
        visible_rows,
        content_len,
        selected_line,
        SCROLL_EDGE_MARGIN,
    );

    let p = Paragraph::new(lines)
        .scroll(content_offset as u16, 0)
        .style(Style::default().bg(props.theme.surface()));
    frame.render_widget(p, area);

    if max_scroll > 0 {
        draw_scrollbar(frame, area, content_offset, max_scroll, props.theme);
    }
}

fn draw_footer(frame: &mut Frame, rect: Rect, focus: ConfigFocus, theme: &Theme) {
    if rect.height == 0 {
        return;
    }

    let bg = theme.body();
    let fill = Style::default().bg(bg);
    let key_style = crate::components::keycap::keycap_style(theme).bg(bg);
    let hint_style = fill.fg(theme.muted());

    let pairs: Vec<(&'static str, &'static str)> = match focus {
        ConfigFocus::Categories => vec![
            ("↑/↓", "select"),
            ("Enter", "enter panel"),
            ("Esc", "close"),
        ],
        ConfigFocus::Detail => vec![
            ("↑/↓", "navigate"),
            ("Enter/Space", "apply/toggle"),
            ("Esc", "back to nav"),
        ],
    };

    const PAIR_GAP: usize = 3;
    const MARGIN_MIN: usize = 2;
    let width = rect.width as usize;

    let total_pair_width: usize = pairs
        .iter()
        .map(|(k, h)| k.width() + 1 + h.width())
        .sum::<usize>()
        + (pairs.len().saturating_sub(1) * PAIR_GAP);

    let count = if total_pair_width + (MARGIN_MIN * 2) <= width {
        pairs.len()
    } else {
        let mut running = 0;
        let mut c = 0;
        for (i, (k, h)) in pairs.iter().enumerate() {
            let pair_w = k.width() + 1 + h.width() + if i > 0 { PAIR_GAP } else { 0 };
            if running + pair_w + (MARGIN_MIN * 2) <= width {
                running += pair_w;
                c += 1;
            } else {
                break;
            }
        }
        c
    };

    let mut spans: Vec<Span<'static>> = Vec::new();
    for (i, (key, hint)) in pairs[..count].iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw("   "));
        }
        spans.push(Span::styled((*key).to_string(), key_style));
        spans.push(Span::raw(" "));
        spans.push(Span::styled((*hint).to_string(), hint_style));
    }

    let p = Paragraph::new(Line::from(spans)).style(fill);
    let row_rect = Rect {
        x: rect.x,
        y: rect.y + 1,
        width: rect.width,
        height: 1,
    };
    frame.render_widget(p, row_rect);
}

pub(super) fn render_scrollable(
    frame: &mut Frame,
    rect: Rect,
    lines: Vec<Line<'static>>,
    scroll: &mut usize,
    selected_line: Option<usize>,
    theme: &Theme,
) {
    let visible_rows = rect.height as usize;
    let content_len = lines.len();

    let (content_offset, max_scroll) = resolve_scroll(
        scroll,
        visible_rows,
        content_len,
        selected_line,
        SCROLL_EDGE_MARGIN,
    );

    let p = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .scroll(content_offset as u16, 0)
        .style(Style::default().bg(theme.body()));
    frame.render_widget(p, rect);

    if max_scroll > 0 {
        draw_scrollbar(frame, rect, content_offset, max_scroll, theme);
    }
}
