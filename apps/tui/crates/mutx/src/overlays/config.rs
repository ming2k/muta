//! Settings View (`/settings`, formerly `/config`): a first-class, full-screen configuration center
//! providing dual-pane (Master-Detail) navigation over all system settings.
//!
//! Layout:
//! ┌─ ⚙ SETTINGS · ~/workspace ────────────────────── Appearance › Themes, palette swatches & custom colors ────┐
//! │                                                                                                            │
//! │ ┌──────────────────────┐ ┌───────────────────────────────────────────────────────────────────────────────┐ │
//! │ │ › ◐ Appearance       │ │ Choose a palette. Presets apply instantly; Custom allows full hex editing.   │ │
//! │ │   ≡ Transcript       │ │                                                                               │ │
//! │ │   ⚙ Behavior         │ │ › ● zen         Dark slate baseline (default)   ■ ■ ■ ■ ■                     │ │
//! │ │   ℹ System & Info    │ │   ○ midnight    Deep obsidian dark              ■ ■ ■ ■ ■                     │ │
//! │ │                      │ │   ○ nord        Arctic blue-gray palette        ■ ■ ■ ■ ■                     │ │
//! │ │                      │ │   ○ catppuccin  Warm pastel mocha palette       ■ ■ ■ ■ ■                     │ │
//! │ │                      │ │   ○ paper       High-contrast warm light        ■ ■ ■ ■ ■                     │ │
//! │ │                      │ │   ○ custom      User-defined palette            ■ ■ ■ ■ ■                     │ │
//! │ └──────────────────────┘ └───────────────────────────────────────────────────────────────────────────────┘ │
//! ├────────────────────────────────────────────────────────────────────────────────────────────────────────────┤
//! │              \[↑/↓\] select   \[Tab\] switch pane   \[Enter\] apply   \[Esc\] close                                │
//! └────────────────────────────────────────────────────────────────────────────────────────────────────────────┘

use muta_contracts::ColorSchemeConfig;
use mutx_engine::{
    Constraint, Direction, Frame, Layout, Line, Modifier, Rect, Span, Style,
    {Block as RtBlock, Clear, Paragraph},
};
use unicode_width::UnicodeWidthStr;

use crate::primitives::{SCROLL_EDGE_MARGIN, draw_scrollbar, resolve_scroll};
use crate::view::{CUSTOM_COLOR_FIELDS, Theme};

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
            ConfigCategory::Appearance => "Themes, palette swatches & custom colors",
            ConfigCategory::Transcript => "Turn bands, auto-scroll & disclosures",
            ConfigCategory::Behavior => "Click-outside dismiss & interaction rules",
            ConfigCategory::WebSearch => "Search providers, page reader & API keys",
            ConfigCategory::System => "Config file paths, runtime & daemon info",
        }
    }

    pub fn icon(self) -> &'static str {
        match self {
            ConfigCategory::Appearance => "◐",
            ConfigCategory::Transcript => "≡",
            ConfigCategory::Behavior => "⚙",
            ConfigCategory::WebSearch => "⌕",
            ConfigCategory::System => "ℹ",
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
    /// composer input row (`Some(4)` = SearXNG URL, `Some(5..=9)` = API
    /// keys). `None` = browse mode.
    pub websearch_editing: Option<usize>,
    pub workspace: &'a str,
    pub category_scroll: &'a mut usize,
    pub detail_scroll: &'a mut usize,
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

    // 4 vertical zones: Top Header (1 row), Blank Line (1 row), Center Body (flexible), Bottom Footer (3 rows).
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
    let body_rect = vertical_chunks[2];
    let footer_rect = vertical_chunks[3];

    let category = ConfigCategory::from_index(props.category_index);

    // 1. Top Header Row (Line 1)
    draw_header(frame, header_rect, props.workspace, category, props.theme);

    // 2. Center Two-Pane Master-Detail Area (starting after blank spacer row)
    let category_width = (body_rect.width / 4).clamp(20, 26);
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

    // 3. Bottom Envoy-Style 3-Row Footer
    draw_footer(
        frame,
        footer_rect,
        props.focus,
        props.custom_editing,
        props.theme,
    );

    ConfigRects {
        area,
        category_body,
        detail_body,
    }
}

// ── Header ─────────────────────────────────────────────────────────────────

fn draw_header(
    frame: &mut Frame,
    rect: Rect,
    workspace: &str,
    category: ConfigCategory,
    theme: &Theme,
) {
    let bg = theme.body();
    let fill = Style::default().bg(bg);
    let brand_style = fill.fg(theme.brand()).add_modifier(Modifier::BOLD);
    let muted_style = fill.fg(theme.muted());

    let left_title = " ⚙ SETTINGS";
    let ws_text = if workspace.is_empty() {
        String::new()
    } else {
        format!(" · {workspace}")
    };

    let cat_title = category.title();
    let cat_sub = category.subtitle();
    let right_len = cat_title.width() + 3 + cat_sub.width() + 1; // "Title › Subtitle "
    let left_len = left_title.width() + ws_text.width();
    let total_w = rect.width as usize;

    let mut spans = vec![
        Span::styled(left_title, brand_style),
        Span::styled(ws_text, muted_style),
    ];

    if total_w > left_len + right_len + 2 {
        let gap = total_w - left_len - right_len;
        spans.push(Span::styled(" ".repeat(gap), fill));
        spans.push(Span::styled(
            cat_title,
            fill.fg(theme.fg()).add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(" › ", fill.fg(theme.dim())));
        spans.push(Span::styled(format!("{cat_sub} "), fill.fg(theme.muted())));
    } else if total_w > left_len + cat_title.width() + 3 {
        let short_len = cat_title.width() + 1;
        let gap = total_w - left_len - short_len;
        spans.push(Span::styled(" ".repeat(gap), fill));
        spans.push(Span::styled(
            format!("{cat_title} "),
            fill.fg(theme.fg()).add_modifier(Modifier::BOLD),
        ));
    } else {
        let gap = total_w.saturating_sub(left_len);
        spans.push(Span::styled(" ".repeat(gap), fill));
    }

    frame.render_widget(Paragraph::new(Line::from(spans)), rect);
}

// ── Panels (Left Categories & Right Detail) ────────────────────────────────

fn draw_panels(
    frame: &mut Frame,
    category_area: Rect,
    detail_area: Rect,
    props: &mut ConfigViewProps<'_>,
    category: ConfigCategory,
) -> (Rect, Rect) {
    let is_cat_focused = props.focus == ConfigFocus::Categories;
    let is_det_focused = props.focus == ConfigFocus::Detail;

    // Left Panel: Categories
    let cat_body = inset_panel(frame, category_area, props.theme);
    draw_category_list(
        frame,
        cat_body,
        props.category_index,
        is_cat_focused,
        props.category_scroll,
        props.theme,
    );

    // Right Panel: Detail
    let det_body = inset_panel(frame, detail_area, props.theme);
    draw_category_detail(frame, det_body, props, category, is_det_focused);

    (cat_body, det_body)
}

fn inset_panel(frame: &mut Frame, area: Rect, theme: &Theme) -> Rect {
    frame.render_widget(
        RtBlock::default().style(Style::default().bg(theme.panel())),
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
        let icon = cat.icon();
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

        let icon_style = Style::default().fg(if is_sel { theme.brand() } else { theme.muted() });

        lines.push(Line::from(vec![
            Span::styled(format!(" {cursor} "), cursor_style),
            Span::styled(format!("{icon} "), icon_style),
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
        "Choose a palette. Presets apply instantly; Custom allows full hex editing.",
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

        let label_w = 12usize;
        let desc_w = (body.width as usize)
            .saturating_sub(6 + label_w + 10 + 4)
            .max(10);
        let desc = if scheme.description.width() > desc_w {
            format!("{}…", &scheme.description[..desc_w.saturating_sub(1)])
        } else {
            scheme.description.to_string()
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
            Span::styled("■", Style::default().fg(colors[0])),
            Span::styled("■", Style::default().fg(colors[1])),
            Span::styled("■", Style::default().fg(colors[2])),
            Span::styled("■", Style::default().fg(colors[3])),
            Span::styled("■", Style::default().fg(colors[4])),
        ]));
    }

    // Custom Scheme Hex Editor (if custom is selected or active)
    let is_custom_sel = (props.detail_index % num_schemes) == (num_schemes - 1);
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
        "── Live Transcript & Components Preview ───────────────────────",
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
            " turn 1 · claude-3-7-sonnet · 15:30".to_string(),
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
        "Modal backdrop click and interaction defaults.",
        Style::default().fg(props.theme.muted()),
    )));
    lines.push(Line::from(""));

    let items = [
        (
            "Click Outside Dismiss",
            if props.click_outside_dismiss {
                "enabled [●]"
            } else {
                "disabled [○]"
            },
            "Click outside a modal backdrop to close it (mirrors Esc)",
        ),
        (
            "Confirmation Mode",
            "always confirm",
            "Ask for confirmation on high-impact external actions",
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
            Span::styled(format!("{:<26}", label), row_style),
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
const WEBSEARCH_BACKENDS: &[(&str, &str)] = &[
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

/// Cycle a reader id through builtin → jina → none.
pub fn cycle_reader(current: &str) -> &'static str {
    match current.trim() {
        "builtin" => "jina",
        "jina" => "none",
        _ => "builtin",
    }
}

#[allow(clippy::too_many_lines)]
fn draw_websearch_detail(
    frame: &mut Frame,
    body: Rect,
    props: &mut ConfigViewProps<'_>,
    focused: bool,
) {
    let _ = frame;
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
        "websearch & webfetch backends. Changes apply live and persist to config.toml.",
        Style::default().fg(props.theme.muted()),
    )));
    lines.push(Line::from(""));

    let backend_desc = |id: &str| -> &str {
        WEBSEARCH_BACKENDS
            .iter()
            .find(|(bid, _)| *bid == id)
            .map(|(_, d)| *d)
            .unwrap_or("unknown backend")
    };

    let searxng_note = if ws.provider == "searxng" || ws.fallback == "searxng" {
        ws.searxng_url
            .as_deref()
            .map(|u| format!("JSON endpoint: {u}"))
            .unwrap_or_else(|| "JSON endpoint not set — searxng will be inactive".to_string())
    } else {
        "only used when a backend is searxng".to_string()
    };

    let key_row = |set: bool| -> String {
        if set {
            "set [●]".to_string()
        } else {
            "not set [○]".to_string()
        }
    };

    let primary_desc = if ws.provider == "none" || ws.provider == "disabled" {
        "disabled — websearch is excluded from model requests".to_string()
    } else if ws.provider == "tavily" && !ws.tavily_api_key_set {
        "inactive — Tavily API key required to enable tool".to_string()
    } else if ws.provider == "bocha" && !ws.bocha_api_key_set {
        "inactive — Bocha API key required to enable tool".to_string()
    } else if ws.provider == "searxng" && ws.searxng_url.as_deref().unwrap_or("").is_empty() {
        "inactive — SearXNG JSON endpoint required to enable tool".to_string()
    } else {
        backend_desc(&ws.provider).to_string()
    };

    let reader_desc = if ws.reader == "none" || ws.reader == "disabled" {
        "disabled — webfetch is excluded from model requests".to_string()
    } else if ws.reader == "jina" {
        "r.jina.ai: JS rendering + readability extraction".to_string()
    } else {
        "direct fetch + local HTML stripping (no JS)".to_string()
    };

    let items: Vec<(String, String, String)> = vec![
        // ── 1. Web Search (Breadth) ──
        (
            "Primary Backend".to_string(),
            ws.provider.clone(),
            primary_desc,
        ),
        (
            "Fallback Backend".to_string(),
            if ws.fallback.trim().is_empty() || ws.fallback == "none" {
                "(none)".to_string()
            } else {
                ws.fallback.clone()
            },
            "tried automatically when the primary fails".to_string(),
        ),
        (
            "SearXNG URL".to_string(),
            ws.searxng_url.clone().unwrap_or_default(),
            searxng_note,
        ),
        (
            "Exa API Key".to_string(),
            key_row(ws.exa_api_key_set),
            "optional — raises the anonymous quota".to_string(),
        ),
        (
            "Parallel API Key".to_string(),
            key_row(ws.parallel_api_key_set),
            "optional — raises the anonymous quota".to_string(),
        ),
        (
            "Tavily API Key".to_string(),
            key_row(ws.tavily_api_key_set),
            "required when the backend is tavily".to_string(),
        ),
        (
            "Bocha API Key".to_string(),
            key_row(ws.bocha_api_key_set),
            "required when the backend is bocha".to_string(),
        ),
        // ── 2. Web Fetch (Depth) ──
        ("Page Reader".to_string(), ws.reader.clone(), reader_desc),
        (
            "Jina Reader Key".to_string(),
            key_row(ws.jina_api_key_set),
            "optional — raises the reader rate limit".to_string(),
        ),
        // ── 3. Shared Network & Timeout ──
        (
            "Timeout".to_string(),
            format!("{} s", ws.timeout_secs),
            "per-request timeout for both tools".to_string(),
        ),
    ];

    let editing_field = props.websearch_editing_field();
    for (i, (label, val, desc)) in items.iter().enumerate() {
        if i == 0 {
            lines.push(Line::from(Span::styled(
                "── 1. Web Search (Breadth) ───────────────────────────",
                Style::default()
                    .fg(props.theme.brand())
                    .add_modifier(Modifier::BOLD),
            )));
            lines.push(Line::from(""));
        } else if i == 7 {
            lines.push(Line::from(Span::styled(
                "── 2. Web Fetch (Depth) ───────────────────────────────",
                Style::default()
                    .fg(props.theme.brand())
                    .add_modifier(Modifier::BOLD),
            )));
            lines.push(Line::from(""));
        } else if i == 9 {
            lines.push(Line::from(Span::styled(
                "── 3. Shared Network & Timeout ───────────────────────",
                Style::default()
                    .fg(props.theme.brand())
                    .add_modifier(Modifier::BOLD),
            )));
            lines.push(Line::from(""));
        }

        let is_sel = i == props.detail_index;
        if is_sel {
            selected_line = Some(lines.len());
        }
        let editing_this = editing_field == Some(i);

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

        let shown: String = if editing_this {
            props.input.to_string()
        } else if i == 2 && val.is_empty() {
            "(not set)".to_string()
        } else {
            val.clone()
        };

        let mut row_spans = vec![
            Span::styled(
                format!(" {cursor} "),
                Style::default().fg(if is_sel {
                    props.theme.brand()
                } else {
                    props.theme.dim()
                }),
            ),
            Span::styled(format!("{:<18}", label), row_style),
            Span::styled("  ", Style::default()),
        ];
        if editing_this {
            // Composer-style cursor rendering inside the settings row.
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
        } else {
            row_spans.push(Span::styled(
                format!("{:<16}", shown),
                Style::default().fg(props.theme.brand()),
            ));
        }
        lines.push(Line::from(row_spans));
        lines.push(Line::from(vec![
            Span::raw("     "),
            Span::styled(desc.clone(), Style::default().fg(props.theme.muted())),
        ]));
        lines.push(Line::from(""));
    }

    lines.push(Line::from(Span::styled(
        "Keys persist to credentials.toml (never config.toml); submitting an \
         empty value clears a key. Proxy: set [websearch] proxy in config.toml.",
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

// ── Footer (Envoy-Style 3-Row Footer) ──────────────────────────────────────

fn draw_footer(
    frame: &mut Frame,
    rect: Rect,
    focus: ConfigFocus,
    custom_editing: bool,
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
    let pairs: Vec<(&'static str, &'static str)> = if custom_editing {
        vec![
            ("↑/↓", "field"),
            ("Enter", "save palette"),
            ("Esc", "cancel"),
        ]
    } else {
        match focus {
            ConfigFocus::Categories => vec![
                ("↑/↓", "select"),
                ("Tab", "edit section"),
                ("Enter", "open"),
                ("Esc", "close"),
            ],
            ConfigFocus::Detail => vec![
                ("↑/↓", "navigate"),
                ("Tab", "categories"),
                ("Enter/Space", "apply/toggle"),
                ("Esc", "back"),
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

    let mut row_spans = vec![Span::styled(" ".repeat(pad_left), fill)];
    row_spans.extend(spans);
    row_spans.push(Span::styled(" ".repeat(pad_right), fill));

    // Paint 3 rows: blank row 1, content row 2, blank row 3.
    let r1 = Rect {
        x: rect.x,
        y: rect.y,
        width: rect.width,
        height: 1,
    };
    let r2 = Rect {
        x: rect.x,
        y: rect.y + 1,
        width: rect.width,
        height: 1,
    };
    let r3 = Rect {
        x: rect.x,
        y: rect.y + 2,
        width: rect.width,
        height: 1,
    };

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(" ".repeat(width), fill))),
        r1,
    );
    frame.render_widget(Paragraph::new(Line::from(row_spans)), r2);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(" ".repeat(width), fill))),
        r3,
    );
}

// ── Shared Helpers ─────────────────────────────────────────────────────────

fn render_scrollable(
    frame: &mut Frame,
    body: Rect,
    lines: Vec<Line<'static>>,
    scroll: &mut usize,
    follow: Option<usize>,
    theme: &Theme,
) {
    let visible = body.height as usize;
    let (_, max_scroll) = resolve_scroll(scroll, visible, lines.len(), follow, SCROLL_EDGE_MARGIN);
    let para = Paragraph::new(lines).scroll(*scroll as u16, 0);
    frame.render_widget(para, body);
    draw_scrollbar(frame, body, *scroll, max_scroll, theme);
}
