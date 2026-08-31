//! Settings View (`/settings`, formerly `/config`): a first-class, full-screen configuration center
//! providing dual-pane (Master-Detail) navigation over all system settings.

use muta_contracts::ColorSchemeConfig;
use mutx_engine::{
    Constraint, Direction, Frame, Layout, Line, Modifier, Rect, Span, Style,
    {Block as RtBlock, Clear, Paragraph},
};
use unicode_width::UnicodeWidthStr;

use crate::primitives::{SCROLL_EDGE_MARGIN, draw_scrollbar, resolve_scroll};
use crate::theme::mix;
use crate::view::{CUSTOM_COLOR_FIELDS, Theme};
use crate::view_header::{
    SettingsHead, ViewHeader, ViewHints, ViewKind, draw_view_header, draw_view_header_hints,
};

/// Which pane of the Settings View currently owns the keyboard.
#[derive(PartialEq, Eq, Clone, Copy, Debug, Default)]
pub enum ConfigFocus {
    /// Left pane: Category navigation (Appearance, Transcript, Behavior, System).
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
            ConfigCategory::WebSearch => "Web Tools",
            ConfigCategory::System => "System & Info",
        }
    }

    pub fn subtitle(self) -> &'static str {
        match self {
            ConfigCategory::Appearance => "Themes & palette swatches",
            ConfigCategory::Transcript => "Turn bands, auto-scroll & disclosures",
            ConfigCategory::Behavior => "Click-outside dismiss & interaction rules",
            ConfigCategory::WebSearch => "Search providers, page reader & API keys",
            ConfigCategory::System => "Config file paths, runtime & daemon info",
        }
    }
}

/// Geometry sub-rects returned by [`draw_config_view`].
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
    pub focus: ConfigFocus,
    pub color_scheme: &'a str,
    pub custom_color_scheme: &'a ColorSchemeConfig,
    pub custom_color_draft: &'a ColorSchemeConfig,
    pub custom_editing: bool,
    pub input: &'a str,
    pub cursor_position: usize,
    pub transcript_layout: crate::view::layout::Strategy,
    pub expand_auto_scroll: bool,
    pub click_outside_dismiss: bool,
    /// Latest `[websearch]` snapshot from the harness (`None` while the
    /// query is still in flight — the pane renders a placeholder).
    pub websearch: Option<&'a muta_contracts::WebSearchConfigView>,
    /// Web-search pane text-editing mode: which field index borrows the
    /// composer input row (`Some(2)` = SearXNG URL, `Some(3..=6 | 8)` = API
    /// keys). `None` = browse mode.
    pub websearch_editing: Option<usize>,
    pub workspace: &'a str,
    pub category_scroll: &'a mut usize,
    pub detail_scroll: &'a mut usize,
    pub breadcrumbs: Option<&'a str>,
    pub theme: &'a Theme,
}

impl ConfigViewProps<'_> {
    /// Which web-search field (if any) is capturing composer input. `None`
    /// means the web-search detail pane is in browse mode. Field indices
    /// match the rows of [`draw_websearch_detail`].
    pub fn websearch_editing_field(&self) -> Option<usize> {
        self.websearch_editing
    }
}

/// Draw the full-screen Settings View.
pub fn draw_config_view(frame: &mut Frame, mut props: ConfigViewProps<'_>) -> ConfigRects {
    let area = frame.area();
    frame.render_widget(Clear, area);

    // Fill the full background with the canvas tone.
    frame.render_widget(
        RtBlock::default().style(Style::default().bg(props.theme.body())),
        area,
    );

    // 4 vertical zones: Top Header (1 row), Breadcrumbs Subhead (1 row), Center Body (flexible), Bottom Footer (3 rows).
    let vertical_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(8),
            Constraint::Length(3),
        ])
        .split(area);

    let header_rect = vertical_chunks[0];
    let subhead_rect = vertical_chunks[1];
    let body_rect = vertical_chunks[2];
    let footer_rect = vertical_chunks[3];

    let category = ConfigCategory::from_index(props.category_index);

    // 1. Top Header Row (Line 1 - Standard View ViewHeader)
    let header = ViewHeader::Settings(&SettingsHead {
        workspace: props.workspace,
        category: category.title(),
        subtitle: category.subtitle(),
    });
    draw_view_header(frame, header_rect, &header, props.theme);

    // 2. View Stack Breadcrumbs & Affordance (Line 2 - Standard ViewHints)
    let view_hints = ViewHints {
        kind: ViewKind::Settings,
        asides: None,
        interruptible: false,
        parent_note: "",
        breadcrumbs: props.breadcrumbs,
    };
    draw_view_header_hints(frame, subhead_rect, &view_hints, props.theme);

    // 3. Center Two-Pane Master-Detail Area (with subtle background contrast)
    let category_width = (body_rect.width / 4).clamp(18, 24);
    let horizontal_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(category_width),
            Constraint::Length(1),
            Constraint::Min(30),
        ])
        .split(body_rect);

    let category_area = horizontal_chunks[0];
    let detail_area = horizontal_chunks[2];

    let (category_body, detail_body) =
        draw_panels(frame, category_area, detail_area, &mut props, category);

    // 4. Bottom Keycap Footer
    draw_footer(
        frame,
        footer_rect,
        props.focus,
        props.custom_editing,
        props.websearch_editing,
        props.theme,
    );

    ConfigRects {
        area,
        category_body,
        detail_body,
    }
}

// ── Panels (Left Nav Sidebar & Right Detail) ───────────────────────────────

fn draw_panels(
    frame: &mut Frame,
    category_area: Rect,
    detail_area: Rect,
    props: &mut ConfigViewProps<'_>,
    category: ConfigCategory,
) -> (Rect, Rect) {
    let is_cat_focused = props.focus == ConfigFocus::Categories;
    let is_det_focused = props.focus == ConfigFocus::Detail;

    // Left Panel: Sidebar tone
    let cat_bg = props.theme.panel();
    let cat_body = inset_panel(frame, category_area, cat_bg);
    draw_category_list(
        frame,
        cat_body,
        props.category_index,
        is_cat_focused,
        props.category_scroll,
        props.theme,
    );

    // Right Panel: Subtle elevated contrast tone
    let det_bg = mix(props.theme.panel(), props.theme.raised(), 0.35);
    let det_body = inset_panel(frame, detail_area, det_bg);
    draw_category_detail(frame, det_body, props, category, is_det_focused);

    (cat_body, det_body)
}

fn inset_panel(frame: &mut Frame, area: Rect, bg: mutx_engine::Color) -> Rect {
    frame.render_widget(
        RtBlock::default().style(Style::default().bg(bg)),
        area,
    );

    // Inner Body (inset by 1 cell on all sides)
    Rect {
        x: area.x + 1,
        y: area.y + 1,
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(2),
    }
}

// ── Categories List Rendering ──────────────────────────────────────────────

fn draw_category_list(
    frame: &mut Frame,
    body: Rect,
    selected_index: usize,
    focused: bool,
    scroll: &mut usize,
    theme: &Theme,
) {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut selected_line = None;

    for (i, cat) in ConfigCategory::ALL.iter().enumerate() {
        let is_sel = i == (selected_index % ConfigCategory::ALL.len());
        if is_sel {
            selected_line = Some(lines.len());
        }

        let cursor = if is_sel { "›" } else { " " };
        let name = cat.title();

        let name_style = if is_sel && focused {
            Style::default()
                .fg(theme.brand())
                .add_modifier(Modifier::BOLD)
        } else if is_sel {
            Style::default().fg(theme.fg()).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.muted())
        };

        let cursor_style = Style::default().fg(if is_sel { theme.brand() } else { theme.dim() });

        // Clean text-only category row without icons
        lines.push(Line::from(vec![
            Span::styled(format!(" {cursor} "), cursor_style),
            Span::styled(name.to_string(), name_style),
        ]));
    }

    render_scrollable(frame, body, lines, scroll, selected_line, theme);
}

// ── Detail Panels ──────────────────────────────────────────────────────────

fn draw_category_detail(
    frame: &mut Frame,
    body: Rect,
    props: &mut ConfigViewProps<'_>,
    category: ConfigCategory,
    focused: bool,
) {
    match category {
        ConfigCategory::Appearance => {
            draw_appearance_detail(frame, body, props, focused);
        }
        ConfigCategory::Transcript => {
            draw_transcript_detail(frame, body, props, focused);
        }
        ConfigCategory::Behavior => {
            draw_behavior_detail(frame, body, props, focused);
        }
        ConfigCategory::WebSearch => {
            draw_websearch_detail(frame, body, props, focused);
        }
        ConfigCategory::System => {
            draw_system_detail(frame, body, props, focused);
        }
    }
}

// 1. Appearance Detail Pane
fn draw_appearance_detail(
    frame: &mut Frame,
    body: Rect,
    props: &mut ConfigViewProps<'_>,
    focused: bool,
) {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let current_norm = Theme::normalize_color_scheme(props.color_scheme);
    let mut selected_line = None;

    lines.push(Line::from(Span::styled(
        "Choose a palette. Up/Down previews instantly; Enter applies.",
        Style::default().fg(props.theme.muted()),
    )));
    lines.push(Line::from(""));

    let schemes = Theme::available_color_schemes();
    let num_schemes = schemes.len();
    for (index, scheme) in schemes.iter().enumerate() {
        let is_sel = index == (props.detail_index % num_schemes);
        let is_active = scheme.id == current_norm;
        if is_sel {
            selected_line = Some(lines.len());
        }

        let cursor = if is_sel { "›" } else { " " };
        let active_mark = if is_active { "●" } else { "○" };
        let colors = Theme::preview_colors(&scheme.id, props.custom_color_scheme);

        let row_style = if is_sel && focused {
            Style::default()
                .fg(props.theme.brand())
                .add_modifier(Modifier::BOLD)
        } else if is_sel {
            Style::default()
                .fg(props.theme.fg())
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(props.theme.fg())
        };

        let label_w = 14usize;
        let desc_w = (body.width as usize)
            .saturating_sub(6 + label_w + 14 + 10)
            .max(10);
        let desc = if scheme.description.width() > desc_w {
            format!("{}…", &scheme.description[..desc_w.saturating_sub(1)])
        } else {
            scheme.description.to_string()
        };

        let tag = if is_active {
            " [active]"
        } else if scheme.is_file {
            " [file]"
        } else {
            ""
        };

        lines.push(Line::from(vec![
            Span::styled(
                format!(" {cursor} "),
                Style::default().fg(if is_sel {
                    props.theme.brand()
                } else {
                    props.theme.dim()
                }),
            ),
            Span::styled(
                format!("{active_mark} "),
                Style::default().fg(if is_active {
                    props.theme.ok()
                } else {
                    props.theme.dim()
                }),
            ),
            Span::styled(format!("{:<label_w$}", scheme.label), row_style),
            Span::styled(
                format!("{:<desc_w$}", desc),
                Style::default().fg(props.theme.muted()),
            ),
            Span::raw("  "),
            Span::styled("■ ", Style::default().fg(colors[0])),
            Span::styled("■ ", Style::default().fg(colors[1])),
            Span::styled("■ ", Style::default().fg(colors[2])),
            Span::styled("■ ", Style::default().fg(colors[3])),
            Span::styled("■", Style::default().fg(colors[4])),
            Span::styled(
                tag,
                Style::default().fg(if is_active {
                    props.theme.ok()
                } else {
                    props.theme.dim()
                }),
            ),
        ]));
    }

    // Custom Scheme Hex Editor (if custom is selected or active)
    let is_custom_sel = (props.detail_index % num_schemes) == (num_schemes - 1)
        && schemes.last().map(|s| s.id == "custom").unwrap_or(false);
    if is_custom_sel || current_norm == "custom" {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "── Custom Palette Fields ──────────────────────────────────────",
            Style::default().fg(props.theme.dim()),
        )));

        for (f_idx, field) in CUSTOM_COLOR_FIELDS.iter().enumerate() {
            let stored =
                Theme::custom_color_value(props.custom_color_draft, f_idx).unwrap_or("#000000");
            let shown = if props.custom_editing {
                props.input
            } else {
                stored
            };
            let swatch = Theme::color_from_hex(shown).unwrap_or(props.theme.panel());

            let is_curr_field = props.custom_editing
                && (f_idx == props.detail_index.saturating_sub(num_schemes).min(7));
            let mut row_spans = vec![
                Span::raw("    "),
                Span::styled(
                    format!("{:<12}", field.label),
                    Style::default().fg(if is_curr_field {
                        props.theme.brand()
                    } else {
                        props.theme.fg()
                    }),
                ),
                Span::styled("  ", Style::default().bg(swatch)),
                Span::raw(" "),
            ];

            if is_curr_field {
                let cursor_pos = props.cursor_position.min(shown.len());
                let (left, right) = shown.split_at(cursor_pos);
                let (mid, right) = if !right.is_empty() {
                    right.split_at(1)
                } else {
                    (" ", "")
                };
                row_spans.push(Span::styled(
                    left.to_string(),
                    Style::default().fg(props.theme.brand()),
                ));
                row_spans.push(Span::styled(
                    mid.to_string(),
                    Style::default()
                        .bg(props.theme.brand())
                        .fg(props.theme.body()),
                ));
                row_spans.push(Span::styled(
                    right.to_string(),
                    Style::default().fg(props.theme.brand()),
                ));
                let pad = 9usize.saturating_sub(shown.len() + if right.is_empty() { 1 } else { 0 });
                if pad > 0 {
                    row_spans.push(Span::raw(" ".repeat(pad)));
                }
            } else {
                row_spans.push(Span::styled(
                    format!("{:<9}", shown),
                    Style::default().fg(props.theme.brand()),
                ));
            }

            row_spans.push(Span::styled(
                format!(" {}", field.hint),
                Style::default().fg(props.theme.dim()),
            ));
            lines.push(Line::from(row_spans));
        }
    }

    // Live Preview Box at the bottom
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "── Live Components Preview ─────────────────────────────────────",
        Style::default().fg(props.theme.dim()),
    )));

    let active_theme = if is_custom_sel {
        Theme::from_color_scheme("custom", props.custom_color_draft)
    } else {
        let scheme_id = &schemes[props.detail_index % num_schemes].id;
        Theme::from_color_scheme(scheme_id, props.custom_color_scheme)
    };

    lines.push(Line::from(vec![
        Span::raw("  "),
        Span::styled("◆", Style::default().fg(active_theme.brand())),
        Span::styled(
            " turn 1  claude-3-7-sonnet                         15:30".to_string(),
            Style::default().fg(active_theme.muted()),
        ),
    ]));
    lines.push(Line::from(vec![
        Span::raw("  "),
        Span::styled("✓ ", Style::default().fg(active_theme.ok())),
        Span::styled(
            "read_file",
            Style::default()
                .fg(active_theme.fg())
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            " apps/tui/crates/mutx/src/main.rs",
            Style::default().fg(active_theme.muted()),
        ),
        Span::styled(" (42 lines)", Style::default().fg(active_theme.dim())),
    ]));
    lines.push(Line::from(vec![
        Span::raw("  "),
        Span::styled("📦 ", Style::default().fg(active_theme.crate_tag())),
        Span::styled(
            "crate",
            Style::default()
                .bg(active_theme.crate_badge())
                .fg(active_theme.crate_tag())
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" muta-contracts", Style::default().fg(active_theme.fg())),
    ]));
    lines.push(Line::from(vec![
        Span::raw("  "),
        Span::styled(
            "  Input: ",
            Style::default()
                .bg(active_theme.input_surface())
                .fg(active_theme.fg()),
        ),
        Span::styled(
            "hello world",
            Style::default()
                .bg(active_theme.input_surface())
                .fg(active_theme.fg()),
        ),
        Span::styled(
            "▍",
            Style::default()
                .bg(active_theme.input_surface())
                .fg(active_theme.caret()),
        ),
    ]));

    render_scrollable(
        frame,
        body,
        lines,
        props.detail_scroll,
        selected_line,
        props.theme,
    );
}

// 2. Transcript Detail Pane
fn draw_transcript_detail(
    frame: &mut Frame,
    body: Rect,
    props: &mut ConfigViewProps<'_>,
    focused: bool,
) {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut selected_line = None;

    lines.push(Line::from(Span::styled(
        "Configure transcript grouping, disclosure expansion & auto-scroll behavior.",
        Style::default().fg(props.theme.muted()),
    )));
    lines.push(Line::from(""));

    let strategy_str = props.transcript_layout.as_str();
    let items = [
        (
            "Layout Strategy",
            strategy_str,
            "Each tool-bearing ReAct turn grouped under a header",
        ),
        (
            "Expand Auto-Scroll",
            if props.expand_auto_scroll {
                "enabled [●]"
            } else {
                "disabled [○]"
            },
            "Keep toggled card comfortably placed in viewport",
        ),
        (
            "Default Expanded: edit_file",
            "true [●]",
            "Show diff inspection cards open by default",
        ),
        (
            "Default Expanded: bash",
            "true [●]",
            "Show command execution cards open by default",
        ),
        (
            "Default Expanded: thinking",
            "false [○]",
            "Keep reasoning trace cards collapsed by default",
        ),
    ];

    for (i, (label, val, desc)) in items.iter().enumerate() {
        let is_sel = i == props.detail_index;
        if is_sel {
            selected_line = Some(lines.len());
        }

        let cursor = if is_sel { "›" } else { " " };
        let row_style = if is_sel && focused {
            Style::default()
                .fg(props.theme.brand())
                .add_modifier(Modifier::BOLD)
        } else if is_sel {
            Style::default()
                .fg(props.theme.fg())
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(props.theme.fg())
        };

        lines.push(Line::from(vec![
            Span::styled(
                format!(" {cursor} "),
                Style::default().fg(if is_sel {
                    props.theme.brand()
                } else {
                    props.theme.dim()
                }),
            ),
            Span::styled(format!("{:<30}", label), row_style),
            Span::styled(
                format!("{:<15}", val),
                Style::default().fg(props.theme.brand()),
            ),
        ]));
        lines.push(Line::from(vec![
            Span::raw("     "),
            Span::styled(desc.to_string(), Style::default().fg(props.theme.muted())),
        ]));
        lines.push(Line::from(""));
    }

    render_scrollable(
        frame,
        body,
        lines,
        props.detail_scroll,
        selected_line,
        props.theme,
    );
}

// 3. Behavior Detail Pane
fn draw_behavior_detail(
    frame: &mut Frame,
    body: Rect,
    props: &mut ConfigViewProps<'_>,
    focused: bool,
) {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut selected_line = None;

    lines.push(Line::from(Span::styled(
        "Control modal auto-dismissal, interaction rules & focus handling.",
        Style::default().fg(props.theme.muted()),
    )));
    lines.push(Line::from(""));

    let items = [(
        "Click Outside Dismiss",
        if props.click_outside_dismiss {
            "enabled [●]"
        } else {
            "disabled [○]"
        },
        "Clicking terminal backdrop closes the active modal layer",
    )];

    for (i, (label, val, desc)) in items.iter().enumerate() {
        let is_sel = i == props.detail_index;
        if is_sel {
            selected_line = Some(lines.len());
        }

        let cursor = if is_sel { "›" } else { " " };
        let row_style = if is_sel && focused {
            Style::default()
                .fg(props.theme.brand())
                .add_modifier(Modifier::BOLD)
        } else if is_sel {
            Style::default()
                .fg(props.theme.fg())
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(props.theme.fg())
        };

        lines.push(Line::from(vec![
            Span::styled(
                format!(" {cursor} "),
                Style::default().fg(if is_sel {
                    props.theme.brand()
                } else {
                    props.theme.dim()
                }),
            ),
            Span::styled(format!("{:<28}", label), row_style),
            Span::styled(
                format!("{:<15}", val),
                Style::default().fg(props.theme.brand()),
            ),
        ]));
        lines.push(Line::from(vec![
            Span::raw("     "),
            Span::styled(desc.to_string(), Style::default().fg(props.theme.muted())),
        ]));
        lines.push(Line::from(""));
    }

    render_scrollable(
        frame,
        body,
        lines,
        props.detail_scroll,
        selected_line,
        props.theme,
    );
}

// 4. Web Tools Detail Pane
//
// Row indices here must stay in sync with the activate handler in
// `event_loop/actions.rs` (`InputAction::ConfigActivate`, category 3):
//   0 Primary Backend   — Enter cycles exa→parallel→duckduckgo→searxng→tavily→bocha→none
//   1 Fallback Backend  — Enter cycles the same list plus "(none)"
//   2 SearXNG URL       — Enter starts/stops inline editing (composer row)
//   3 Exa API Key       — Enter starts/stops inline editing; empty submit clears
//   4 Parallel API Key  — Enter starts/stops inline editing; empty submit clears
//   5 Tavily API Key    — Enter starts/stops inline editing; empty submit clears
//   6 Bocha API Key     — Enter starts/stops inline editing; empty submit clears
//   7 Page Reader       — Enter cycles builtin→jina→none
//   8 Jina Reader Key   — Enter starts/stops inline editing; empty submit clears
//   9 Timeout           — Enter +5s (min 5s)
pub const WEBSEARCH_BACKENDS: &[(&str, &str)] = &[
    ("exa", "hosted MCP, anonymous by default (default)"),
    ("parallel", "hosted MCP, anonymous by default"),
    ("duckduckgo", "keyless scraping, frequently blocked"),
    ("searxng", "self-hosted, keyless, needs a URL"),
    ("tavily", "hosted, needs a Tavily key"),
    ("bocha", "hosted AI search, needs a key; China-direct"),
    ("none", "disabled — tool is excluded from model requests"),
];

/// Cycle a backend id through the known list (unknown → exa). Shared by the
/// Settings pane's activate handler for both primary and fallback rows.
pub fn cycle_websearch_backend(current: &str) -> &'static str {
    let idx = WEBSEARCH_BACKENDS.iter().position(|(id, _)| *id == current);
    let next = match idx {
        Some(i) => (i + 1) % WEBSEARCH_BACKENDS.len(),
        None => 0,
    };
    WEBSEARCH_BACKENDS[next].0
}

/// Cycle a reader id through jina → none.
pub fn cycle_reader(current: &str) -> &'static str {
    match current.trim() {
        "jina" => "none",
        _ => "jina",
    }
}

#[allow(clippy::too_many_lines)]
fn draw_websearch_detail(
    frame: &mut Frame,
    body: Rect,
    props: &mut ConfigViewProps<'_>,
    focused: bool,
) {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut selected_line = None;

    let Some(ws) = props.websearch else {
        lines.push(Line::from(Span::styled(
            "Loading web tools configuration…",
            Style::default().fg(props.theme.muted()),
        )));
        render_scrollable(frame, body, lines, props.detail_scroll, None, props.theme);
        return;
    };

    lines.push(Line::from(Span::styled(
        "websearch & webfetch backends. Changes apply live and persist to config.",
        Style::default().fg(props.theme.muted()),
    )));
    lines.push(Line::from(""));

    let backend_badge = |id: &str| -> (&'static str, &'static str) {
        match id {
            "exa" => ("exa", "● Hosted MCP · Anonymous quota default"),
            "parallel" => ("parallel", "● Hosted MCP · Anonymous quota default"),
            "duckduckgo" => ("duckduckgo", "● Keyless Direct · Scraping"),
            "searxng" => ("searxng", "● Self-Hosted · JSON endpoint required"),
            "tavily" => ("tavily", "● Hosted · API Key required"),
            "bocha" => ("bocha", "● Hosted · China-direct AI search"),
            "none" => ("none", "○ Disabled · Excluded from model tools"),
            _ => ("unknown", "○ Unknown backend"),
        }
    };

    let key_status_badge = |set: bool, required: bool| -> (&'static str, mutx_engine::Color) {
        if set {
            ("● Configured", props.theme.ok())
        } else if required {
            ("⚠ Key Required", props.theme.warn())
        } else {
            ("○ Not set (optional)", props.theme.dim())
        }
    };

    let editing_field = props.websearch_editing_field();

    // Section 1: Web Search (Breadth)
    lines.push(Line::from(Span::styled(
        "── 1. Web Search (Breadth) ───────────────────────────",
        Style::default()
            .fg(props.theme.brand())
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(""));

    // Item 0: Primary Backend
    {
        let i = 0;
        let is_sel = i == props.detail_index;
        if is_sel {
            selected_line = Some(lines.len());
        }
        let cursor = if is_sel { "›" } else { " " };
        let (val, desc) = backend_badge(&ws.provider);
        let row_style = if is_sel && focused {
            Style::default().fg(props.theme.brand()).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(props.theme.fg()).add_modifier(Modifier::BOLD)
        };

        lines.push(Line::from(vec![
            Span::styled(format!(" {cursor} "), Style::default().fg(if is_sel { props.theme.brand() } else { props.theme.dim() })),
            Span::styled("Primary Backend   ", row_style),
            Span::styled(format!("[ {:<10} ]", val), Style::default().fg(props.theme.brand()).add_modifier(Modifier::BOLD)),
            Span::raw("  "),
            Span::styled(desc, Style::default().fg(if ws.provider == "none" { props.theme.dim() } else { props.theme.ok() })),
        ]));
        if is_sel && focused {
            lines.push(Line::from(vec![
                Span::raw("     "),
                Span::styled("Options: exa · parallel · duckduckgo · searxng · tavily · bocha · none", Style::default().fg(props.theme.dim())),
                Span::styled("  [Enter/Space to cycle]", Style::default().fg(props.theme.muted())),
            ]));
        }
        lines.push(Line::from(""));
    }

    // Item 1: Fallback Backend
    {
        let i = 1;
        let is_sel = i == props.detail_index;
        if is_sel {
            selected_line = Some(lines.len());
        }
        let cursor = if is_sel { "›" } else { " " };
        let fallback_val = if ws.fallback.trim().is_empty() || ws.fallback == "none" { "(none)" } else { ws.fallback.as_str() };
        let row_style = if is_sel && focused {
            Style::default().fg(props.theme.brand()).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(props.theme.fg()).add_modifier(Modifier::BOLD)
        };

        lines.push(Line::from(vec![
            Span::styled(format!(" {cursor} "), Style::default().fg(if is_sel { props.theme.brand() } else { props.theme.dim() })),
            Span::styled("Fallback Backend  ", row_style),
            Span::styled(format!("[ {:<10} ]", fallback_val), Style::default().fg(props.theme.brand())),
            Span::raw("  "),
            Span::styled("Automatic fallback when primary query fails", Style::default().fg(props.theme.muted())),
        ]));
        if is_sel && focused {
            lines.push(Line::from(vec![
                Span::raw("     "),
                Span::styled("Options: (none) · exa · parallel · duckduckgo · searxng · tavily · bocha", Style::default().fg(props.theme.dim())),
                Span::styled("  [Enter/Space to cycle]", Style::default().fg(props.theme.muted())),
            ]));
        }
        lines.push(Line::from(""));
    }

    // Item 2: SearXNG URL
    {
        let i = 2;
        let is_sel = i == props.detail_index;
        if is_sel {
            selected_line = Some(lines.len());
        }
        let is_editing = editing_field == Some(i);
        let cursor = if is_sel { "›" } else { " " };
        let row_style = if is_sel && focused {
            Style::default().fg(props.theme.brand()).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(props.theme.fg()).add_modifier(Modifier::BOLD)
        };

        let url_display = if is_editing {
            props.input.to_string()
        } else {
            ws.searxng_url.clone().unwrap_or_else(|| "(not configured)".to_string())
        };

        let mut row_spans = vec![
            Span::styled(format!(" {cursor} "), Style::default().fg(if is_sel { props.theme.brand() } else { props.theme.dim() })),
            Span::styled("SearXNG URL       ", row_style),
        ];

        if is_editing {
            render_inline_input(&mut row_spans, &url_display, props.cursor_position, props.theme);
        } else {
            row_spans.push(Span::styled(format!("[ {url_display} ]"), Style::default().fg(if ws.searxng_url.is_some() { props.theme.fg() } else { props.theme.dim() })));
        }

        lines.push(Line::from(row_spans));
        lines.push(Line::from(vec![
            Span::raw("     "),
            Span::styled("Self-hosted JSON endpoint (e.g. http://localhost:8080)", Style::default().fg(props.theme.muted())),
            Span::styled(if is_editing { "  [Enter save · Esc cancel]" } else { "  [Enter to edit]" }, Style::default().fg(props.theme.dim())),
        ]));
        lines.push(Line::from(""));
    }

    // Items 3..=6: API Keys (Exa, Parallel, Tavily, Bocha)
    let key_items = [
        (3, "Exa API Key", ws.exa_api_key_set, ws.provider == "exa" || ws.fallback == "exa", "Optional key to raise anonymous rate quota"),
        (4, "Parallel API Key", ws.parallel_api_key_set, ws.provider == "parallel" || ws.fallback == "parallel", "Optional key to raise anonymous rate quota"),
        (5, "Tavily API Key", ws.tavily_api_key_set, ws.provider == "tavily" || ws.fallback == "tavily", "Required when using Tavily search backend"),
        (6, "Bocha API Key", ws.bocha_api_key_set, ws.provider == "bocha" || ws.fallback == "bocha", "Required when using Bocha search backend"),
    ];

    for (i, label, is_set, is_active_provider, hint) in key_items {
        let is_sel = i == props.detail_index;
        if is_sel {
            selected_line = Some(lines.len());
        }
        let is_editing = editing_field == Some(i);
        let cursor = if is_sel { "›" } else { " " };
        let row_style = if is_sel && focused {
            Style::default().fg(props.theme.brand()).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(props.theme.fg()).add_modifier(Modifier::BOLD)
        };

        let (status_text, status_color) = key_status_badge(is_set, is_active_provider);

        let mut row_spans = vec![
            Span::styled(format!(" {cursor} "), Style::default().fg(if is_sel { props.theme.brand() } else { props.theme.dim() })),
            Span::styled(format!("{:<18}", label), row_style),
        ];

        if is_editing {
            render_inline_input(&mut row_spans, props.input, props.cursor_position, props.theme);
        } else {
            row_spans.push(Span::styled(format!("{:<22}", status_text), Style::default().fg(status_color)));
        }

        lines.push(Line::from(row_spans));
        lines.push(Line::from(vec![
            Span::raw("     "),
            Span::styled(hint.to_string(), Style::default().fg(props.theme.muted())),
            Span::styled(if is_editing { "  [Enter save · Empty Enter clear · Esc cancel]" } else { "  [Enter to configure]" }, Style::default().fg(props.theme.dim())),
        ]));
        lines.push(Line::from(""));
    }

    // Section 2: Web Fetch (Depth)
    lines.push(Line::from(Span::styled(
        "── 2. Web Fetch (Depth) ───────────────────────────────",
        Style::default()
            .fg(props.theme.brand())
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(""));

    // Item 7: Page Reader
    {
        let i = 7;
        let is_sel = i == props.detail_index;
        if is_sel {
            selected_line = Some(lines.len());
        }
        let cursor = if is_sel { "›" } else { " " };
        let reader_val = ws.reader.as_str();
        let reader_desc = match reader_val {
            "jina" => "● r.jina.ai: Readability & Markdown extraction",
            "builtin" => "● Direct fetch + HTML stripping (no JS)",
            "none" => "○ Disabled · Excluded from model tools",
            _ => "● Custom page reader",
        };
        let row_style = if is_sel && focused {
            Style::default().fg(props.theme.brand()).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(props.theme.fg()).add_modifier(Modifier::BOLD)
        };

        lines.push(Line::from(vec![
            Span::styled(format!(" {cursor} "), Style::default().fg(if is_sel { props.theme.brand() } else { props.theme.dim() })),
            Span::styled("Page Reader       ", row_style),
            Span::styled(format!("[ {:<10} ]", reader_val), Style::default().fg(props.theme.brand()).add_modifier(Modifier::BOLD)),
            Span::raw("  "),
            Span::styled(reader_desc, Style::default().fg(if reader_val == "none" { props.theme.dim() } else { props.theme.ok() })),
        ]));
        if is_sel && focused {
            lines.push(Line::from(vec![
                Span::raw("     "),
                Span::styled("Options: jina · builtin · none", Style::default().fg(props.theme.dim())),
                Span::styled("  [Enter/Space to cycle]", Style::default().fg(props.theme.muted())),
            ]));
        }
        lines.push(Line::from(""));
    }

    // Item 8: Jina Reader Key
    {
        let i = 8;
        let is_sel = i == props.detail_index;
        if is_sel {
            selected_line = Some(lines.len());
        }
        let is_editing = editing_field == Some(i);
        let cursor = if is_sel { "›" } else { " " };
        let row_style = if is_sel && focused {
            Style::default().fg(props.theme.brand()).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(props.theme.fg()).add_modifier(Modifier::BOLD)
        };

        let (status_text, status_color) = key_status_badge(ws.jina_api_key_set, false);

        let mut row_spans = vec![
            Span::styled(format!(" {cursor} "), Style::default().fg(if is_sel { props.theme.brand() } else { props.theme.dim() })),
            Span::styled("Jina Reader Key   ", row_style),
        ];

        if is_editing {
            render_inline_input(&mut row_spans, props.input, props.cursor_position, props.theme);
        } else {
            row_spans.push(Span::styled(format!("{:<22}", status_text), Style::default().fg(status_color)));
        }

        lines.push(Line::from(row_spans));
        lines.push(Line::from(vec![
            Span::raw("     "),
            Span::styled("Optional key to raise rate limits on r.jina.ai", Style::default().fg(props.theme.muted())),
            Span::styled(if is_editing { "  [Enter save · Empty Enter clear · Esc cancel]" } else { "  [Enter to configure]" }, Style::default().fg(props.theme.dim())),
        ]));
        lines.push(Line::from(""));
    }

    // Section 3: Shared Network & Timeout
    lines.push(Line::from(Span::styled(
        "── 3. Shared Network & Timeout ───────────────────────",
        Style::default()
            .fg(props.theme.brand())
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(""));

    // Item 9: Timeout
    {
        let i = 9;
        let is_sel = i == props.detail_index;
        if is_sel {
            selected_line = Some(lines.len());
        }
        let cursor = if is_sel { "›" } else { " " };
        let row_style = if is_sel && focused {
            Style::default().fg(props.theme.brand()).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(props.theme.fg()).add_modifier(Modifier::BOLD)
        };

        lines.push(Line::from(vec![
            Span::styled(format!(" {cursor} "), Style::default().fg(if is_sel { props.theme.brand() } else { props.theme.dim() })),
            Span::styled("Request Timeout   ", row_style),
            Span::styled(format!("[ {:>2} s ]", ws.timeout_secs), Style::default().fg(props.theme.brand()).add_modifier(Modifier::BOLD)),
            Span::raw("  "),
            Span::styled("Per-request timeout for search and fetch operations", Style::default().fg(props.theme.muted())),
        ]));
        if is_sel && focused {
            lines.push(Line::from(vec![
                Span::raw("     "),
                Span::styled("[Enter to +5s (min 5s, max 120s)]", Style::default().fg(props.theme.dim())),
            ]));
        }
        lines.push(Line::from(""));
    }

    lines.push(Line::from(Span::styled(
        "Keys persist to credentials.toml (never config.toml); empty submit clears a key.",
        Style::default().fg(props.theme.dim()),
    )));

    render_scrollable(
        frame,
        body,
        lines,
        props.detail_scroll,
        selected_line,
        props.theme,
    );
}

fn render_inline_input(
    spans: &mut Vec<Span<'static>>,
    shown: &str,
    cursor_position: usize,
    theme: &Theme,
) {
    let cursor_pos = cursor_position.min(shown.len());
    let (left, right) = shown.split_at(cursor_pos);
    let (mid, right) = if !right.is_empty() {
        right.split_at(1)
    } else {
        (" ", "")
    };
    spans.push(Span::styled(
        format!("[ {left}"),
        Style::default().fg(theme.brand()),
    ));
    spans.push(Span::styled(
        mid.to_string(),
        Style::default()
            .bg(theme.brand())
            .fg(theme.body()),
    ));
    spans.push(Span::styled(
        format!("{right} ]"),
        Style::default().fg(theme.brand()),
    ));
}

// 5. System Detail Pane
fn draw_system_detail(
    frame: &mut Frame,
    body: Rect,
    props: &mut ConfigViewProps<'_>,
    _focused: bool,
) {
    let mut lines: Vec<Line<'static>> = Vec::new();

    lines.push(Line::from(Span::styled(
        "Active environment and configuration paths.",
        Style::default().fg(props.theme.muted()),
    )));
    lines.push(Line::from(""));

    let items = [
        ("Config File", "~/.config/muta/config.toml"),
        (
            "Workspace",
            if props.workspace.is_empty() {
                "(none)"
            } else {
                props.workspace
            },
        ),
        ("TUI Engine", "In-House Grid-Diff Engine (ADR-0038)"),
        ("Version", env!("CARGO_PKG_VERSION")),
    ];

    for (label, val) in items {
        lines.push(Line::from(vec![
            Span::raw("   "),
            Span::styled(
                format!("{:<16}", label),
                Style::default().fg(props.theme.muted()),
            ),
            Span::styled(
                val.to_string(),
                Style::default()
                    .fg(props.theme.fg())
                    .add_modifier(Modifier::BOLD),
            ),
        ]));
        lines.push(Line::from(""));
    }

    render_scrollable(frame, body, lines, props.detail_scroll, None, props.theme);
}

// ── Footer (Runner-Style 3-Row Footer) ──────────────────────────────────────

fn draw_footer(
    frame: &mut Frame,
    rect: Rect,
    focus: ConfigFocus,
    custom_editing: bool,
    websearch_editing: Option<usize>,
    theme: &Theme,
) {
    if rect.height == 0 {
        return;
    }

    let bg = theme.body();
    let fill = Style::default().bg(bg);
    let key_style = crate::components::keycap::keycap_style(theme).bg(bg);
    let hint_style = fill.fg(theme.muted());

    // Context-sensitive keycap pairs.
    let pairs: Vec<(&'static str, &'static str)> = if websearch_editing.is_some() {
        vec![
            ("Enter", "save"),
            ("Empty + Enter", "clear"),
            ("Esc", "cancel"),
        ]
    } else if custom_editing {
        vec![
            ("↑/↓", "field"),
            ("Enter", "save palette"),
            ("Esc", "cancel"),
        ]
    } else {
        match focus {
            ConfigFocus::Categories => vec![
                ("↑/↓", "select"),
                ("Enter", "enter panel"),
                ("Esc", "close"),
            ],
            ConfigFocus::Detail => vec![
                ("↑/↓", "navigate"),
                ("Enter/Space", "apply/toggle/edit"),
                ("Esc", "back to nav"),
            ],
        }
    };

    const PAIR_GAP: usize = 3;
    const MARGIN_MIN: usize = 2;
    let width = rect.width as usize;

    // Filter pairs that fit.
    let content: Vec<(&'static str, &'static str)> = {
        let mut chosen = pairs.clone();
        loop {
            let pairs_width: usize = chosen
                .iter()
                .map(|(key, label)| key.width() + 1 + label.width())
                .sum();
            let needed = pairs_width + PAIR_GAP * chosen.len().saturating_sub(1);
            if needed <= width.saturating_sub(2 * MARGIN_MIN) || chosen.len() <= 1 {
                break;
            }
            chosen.pop();
        }
        chosen
    };

    let mut spans: Vec<Span<'static>> = Vec::new();
    for (idx, (key, label)) in content.iter().enumerate() {
        if idx > 0 {
            spans.push(Span::styled(" ".repeat(PAIR_GAP), fill));
        }
        spans.push(Span::styled(key.to_string(), key_style));
        spans.push(Span::styled(format!(" {label}"), hint_style));
    }

    let content_len: usize = content
        .iter()
        .map(|(k, l)| k.width() + 1 + l.width())
        .sum::<usize>()
        + PAIR_GAP * content.len().saturating_sub(1);

    let pad_left = (width.saturating_sub(content_len)) / 2;
    let pad_right = width.saturating_sub(pad_left + content_len);

    let mut line_spans = Vec::new();
    if pad_left > 0 {
        line_spans.push(Span::styled(" ".repeat(pad_left), fill));
    }
    line_spans.extend(spans);
    if pad_right > 0 {
        line_spans.push(Span::styled(" ".repeat(pad_right), fill));
    }

    // Centered 1-line legend padded with top/bottom empty rows for 3-row footer.
    let footer_lines = vec![
        Line::from(Span::styled(" ".repeat(width), fill)),
        Line::from(line_spans),
        Line::from(Span::styled(" ".repeat(width), fill)),
    ];

    frame.render_widget(Paragraph::new(footer_lines), rect);
}

// ── Shared Scroll Helper ───────────────────────────────────────────────────

fn render_scrollable(
    frame: &mut Frame,
    area: Rect,
    lines: Vec<Line<'static>>,
    scroll: &mut usize,
    selected_line: Option<usize>,
    theme: &Theme,
) {
    let total = lines.len();
    let viewport = area.height as usize;
    let (start, max_scroll) = resolve_scroll(scroll, viewport, total, selected_line, SCROLL_EDGE_MARGIN);

    let visible: Vec<Line<'static>> = lines
        .into_iter()
        .skip(start)
        .take(viewport)
        .collect();

    frame.render_widget(Paragraph::new(visible), area);

    if max_scroll > 0 && area.width > 1 {
        draw_scrollbar(frame, area, start, max_scroll, theme);
    }
}
