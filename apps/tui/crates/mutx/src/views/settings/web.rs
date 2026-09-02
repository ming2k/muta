//! Web settings panel: orthogonal Search (breadth) and Reader (depth) segment control and connection routing.

use mutx_engine::{Frame, Line, Modifier, Rect, Span, Style};

use super::{ConfigViewProps, render_scrollable};

/// Build a floating dropdown picker for Web Search backends.
pub fn build_websearch_provider_dropdown(
    current: &str,
    ws: Option<&muta_contracts::WebSearchConfigView>,
) -> crate::components::dropdown::DropdownState<String> {
    use crate::components::dropdown::{DropdownIndicator, DropdownItem, DropdownState};

    let mut items = Vec::new();

    if let Some(ws) = ws
        && !ws.search_connections.is_empty()
    {
        for conn in &ws.search_connections {
            let is_default = conn.id == "exa-default" || conn.id == "exa";
            let badge = if is_default {
                Some("Default")
            } else if conn.preset_id.is_none() {
                Some("Custom")
            } else {
                None
            };

            let indicator = match conn.preset_id.as_deref().unwrap_or(&conn.id) {
                "tavily" => {
                    if ws.tavily_api_key_set || conn.api_key_env.is_some() {
                        DropdownIndicator::Ready
                    } else {
                        DropdownIndicator::Warning
                    }
                }
                "bocha" => {
                    if ws.bocha_api_key_set || conn.api_key_env.is_some() {
                        DropdownIndicator::Ready
                    } else {
                        DropdownIndicator::Warning
                    }
                }
                "searxng" => {
                    if ws.searxng_url.is_some() || conn.base_url.is_some() {
                        DropdownIndicator::Ready
                    } else {
                        DropdownIndicator::Warning
                    }
                }
                _ => DropdownIndicator::Ready,
            };

            let description = if let Some(preset) = conn
                .preset_id
                .as_deref()
                .and_then(muta_contracts::WebSearchPresets::find)
            {
                preset.description.to_string()
            } else if let Some(url) = &conn.base_url {
                format!("Endpoint: {url}")
            } else {
                format!("Connection: {}", conn.id)
            };

            let mut item = DropdownItem::new(conn.id.clone(), conn.display_name(), conn.id.clone())
                .with_description(description)
                .with_indicator(indicator);

            if let Some(b) = badge {
                item = item.with_badge(b);
            }
            items.push(item);
        }
    } else {
        items.push(
            DropdownItem::new("exa", "Exa Search", "exa".to_string())
                .with_description("Hosted MCP · Anonymous quota default")
                .with_badge("Default")
                .with_indicator(DropdownIndicator::Ready),
        );
        items.push(
            DropdownItem::new("parallel", "Parallel Search", "parallel".to_string())
                .with_description("Hosted MCP · Anonymous quota default")
                .with_indicator(DropdownIndicator::Ready),
        );
        items.push(
            DropdownItem::new("tavily", "Tavily AI Search", "tavily".to_string())
                .with_description("Hosted API · Requires Tavily key in credentials")
                .with_indicator(if ws.map(|w| w.tavily_api_key_set).unwrap_or(false) {
                    DropdownIndicator::Ready
                } else {
                    DropdownIndicator::Warning
                }),
        );
        items.push(
            DropdownItem::new("bocha", "Bocha AI Search", "bocha".to_string())
                .with_description("Hosted AI Search · China-direct endpoint")
                .with_indicator(if ws.map(|w| w.bocha_api_key_set).unwrap_or(false) {
                    DropdownIndicator::Ready
                } else {
                    DropdownIndicator::Warning
                }),
        );
        items.push(
            DropdownItem::new("searxng", "SearXNG", "searxng".to_string())
                .with_description("Self-Hosted · Requires JSON search endpoint")
                .with_indicator(if ws.and_then(|w| w.searxng_url.as_ref()).is_some() {
                    DropdownIndicator::Ready
                } else {
                    DropdownIndicator::Warning
                }),
        );
        items.push(
            DropdownItem::new("duckduckgo", "DuckDuckGo", "duckduckgo".to_string())
                .with_description("Keyless Direct Scraping (Rate limited)")
                .with_indicator(DropdownIndicator::Ready),
        );
    }

    items.push(
        DropdownItem::new("none", "Disabled", "none".to_string())
            .with_description("Disable web search tool")
            .with_indicator(DropdownIndicator::Inactive),
    );

    items.push(
        DropdownItem::new(
            "add_new",
            "＋ Add Search Connection...",
            "add_new".to_string(),
        )
        .with_description("Declare a new search connection from preset or custom URL")
        .with_indicator(DropdownIndicator::Ready),
    );

    let mut state = DropdownState::new(Some("Select Web Search Connection"), items)
        .with_context("websearch_provider");
    state.select_by_id(current);
    state
}

/// Build a floating dropdown picker for Web Reader page readers.
pub fn build_websearch_reader_dropdown(
    current: &str,
    ws: Option<&muta_contracts::WebSearchConfigView>,
) -> crate::components::dropdown::DropdownState<String> {
    use crate::components::dropdown::{DropdownIndicator, DropdownItem, DropdownState};

    let mut items = Vec::new();

    if let Some(ws) = ws
        && !ws.reader_connections.is_empty()
    {
        for conn in &ws.reader_connections {
            let is_custom = conn.preset_id.is_none();
            let badge = if is_custom { Some("Custom") } else { None };

            let description = if let Some(preset) = conn
                .preset_id
                .as_deref()
                .and_then(muta_contracts::WebReaderPresets::find)
            {
                preset.description.to_string()
            } else if let Some(url) = &conn.base_url {
                format!("Endpoint: {url}")
            } else {
                format!("Reader: {}", conn.id)
            };

            let mut item = DropdownItem::new(conn.id.clone(), conn.display_name(), conn.id.clone())
                .with_description(description)
                .with_indicator(DropdownIndicator::Ready);

            if let Some(b) = badge {
                item = item.with_badge(b);
            }
            items.push(item);
        }
    }

    items.push(
        DropdownItem::new("none", "Disabled", "none".to_string())
            .with_description("Disable web reader tool")
            .with_indicator(DropdownIndicator::Inactive),
    );

    items.push(
        DropdownItem::new(
            "add_new",
            "＋ Add Reader Connection...",
            "add_new".to_string(),
        )
        .with_description("Declare a new reader connection from preset or custom URL")
        .with_indicator(DropdownIndicator::Ready),
    );

    let mut state =
        DropdownState::new(Some("Select Web Reader"), items).with_context("websearch_reader");
    state.select_by_id(current);
    state
}

/// Build a floating dropdown picker to choose a preset when adding a connection.
pub fn build_add_web_connection_dropdown(
    segment: usize,
) -> crate::components::dropdown::DropdownState<String> {
    use crate::components::dropdown::{DropdownIndicator, DropdownItem, DropdownState};

    let items = if segment == 0 {
        vec![
            DropdownItem::new("exa", "Exa Search (Hosted MCP)", "exa".to_string())
                .with_description("Hosted MCP AI Search · Keyless anonymous or API key")
                .with_badge("Search")
                .with_indicator(DropdownIndicator::Ready),
            DropdownItem::new("tavily", "Tavily AI Search", "tavily".to_string())
                .with_description("Hosted search API tailored for LLM agents")
                .with_badge("Search")
                .with_indicator(DropdownIndicator::Ready),
            DropdownItem::new("bocha", "Bocha AI Search", "bocha".to_string())
                .with_description("Hosted AI Search · China-direct endpoint")
                .with_badge("Search")
                .with_indicator(DropdownIndicator::Ready),
            DropdownItem::new("searxng", "SearXNG (Self-Hosted)", "searxng".to_string())
                .with_description("Self-hosted meta-search engine JSON endpoint")
                .with_badge("Search")
                .with_indicator(DropdownIndicator::Ready),
            DropdownItem::new(
                "parallel",
                "Parallel Search (Hosted MCP)",
                "parallel".to_string(),
            )
            .with_description("Hosted MCP Search · Keyless anonymous or API key")
            .with_badge("Search")
            .with_indicator(DropdownIndicator::Ready),
            DropdownItem::new(
                "custom-search",
                "Custom Search Relay",
                "custom-search".to_string(),
            )
            .with_description("Custom search endpoint (REST / JSON API)")
            .with_badge("Custom")
            .with_indicator(DropdownIndicator::Ready),
        ]
    } else {
        vec![
            DropdownItem::new("jina", "Jina Reader", "jina".to_string())
                .with_description("Server-side JS rendering & Markdown conversion (r.jina.ai)")
                .with_badge("Reader")
                .with_indicator(DropdownIndicator::Ready),
            DropdownItem::new("firecrawl", "Firecrawl Web Reader", "firecrawl".to_string())
                .with_description("Hosted or self-hosted web scraping engine")
                .with_badge("Reader")
                .with_indicator(DropdownIndicator::Ready),
            DropdownItem::new(
                "custom-reader",
                "Custom Web Reader",
                "custom-reader".to_string(),
            )
            .with_description("Custom reader / crawler endpoint (e.g. Firecrawl / Crawl4AI)")
            .with_badge("Custom")
            .with_indicator(DropdownIndicator::Ready),
        ]
    };

    DropdownState::new(
        Some(if segment == 0 {
            "Choose Search Connection Preset"
        } else {
            "Choose Reader Connection Preset"
        }),
        items,
    )
    .with_context(if segment == 0 {
        "add_search_connection"
    } else {
        "add_reader_connection"
    })
}

/// Number of keyboard-selectable rows in the Web Search panel.
pub fn search_item_count(ws: Option<&muta_contracts::WebSearchConfigView>) -> usize {
    3 + ws.map(|ws| ws.search_connections.len()).unwrap_or(0)
}

/// Number of keyboard-selectable rows in the Web Reader panel.
pub fn reader_item_count(ws: Option<&muta_contracts::WebSearchConfigView>) -> usize {
    3 + ws.map(|ws| ws.reader_connections.len()).unwrap_or(0)
}

pub(super) fn draw_search_detail(
    frame: &mut Frame,
    body: Rect,
    props: &mut ConfigViewProps<'_>,
    focused: bool,
) {
    draw_web_detail(frame, body, props, focused, WebPanel::Search);
}

pub(super) fn draw_reader_detail(
    frame: &mut Frame,
    body: Rect,
    props: &mut ConfigViewProps<'_>,
    focused: bool,
) {
    draw_web_detail(frame, body, props, focused, WebPanel::Reader);
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum WebPanel {
    Search,
    Reader,
}

fn draw_web_detail(
    frame: &mut Frame,
    body: Rect,
    props: &mut ConfigViewProps<'_>,
    focused: bool,
    panel: WebPanel,
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

    let (active_id, active_name, route_label, connections_len) = match panel {
        WebPanel::Search => (
            ws.provider.as_str(),
            search_display_name(ws),
            "Search route",
            ws.search_connections.len(),
        ),
        WebPanel::Reader => (
            ws.reader.as_str(),
            reader_display_name(ws),
            "Reader route",
            ws.reader_connections.len(),
        ),
    };

    section_heading(
        &mut lines,
        "ROUTING",
        match panel {
            WebPanel::Search => "Used by search_web to discover sources",
            WebPanel::Reader => "Used by read_url to turn a page into readable content",
        },
        props,
    );
    push_setting_row(
        &mut lines,
        &mut selected_line,
        0,
        props.detail_index,
        focused,
        route_label,
        &active_name,
        if active_id == "none" {
            "Disabled"
        } else {
            "Active"
        },
        "Enter to choose a connection",
        props,
    );

    section_heading(
        &mut lines,
        "REQUEST POLICY",
        "Shared by Search and Reader",
        props,
    );
    push_setting_row(
        &mut lines,
        &mut selected_line,
        1,
        props.detail_index,
        focused,
        "Timeout",
        &format!("{} seconds", ws.timeout_secs),
        "Shared",
        "Enter to increase by 5 seconds (wraps after 120)",
        props,
    );
    lines.push(Line::from(vec![
        Span::raw("     "),
        Span::styled("Proxy", Style::default().fg(props.theme.muted())),
        Span::raw("             "),
        Span::styled(
            ws.proxy
                .as_deref()
                .unwrap_or("Direct connection")
                .to_string(),
            Style::default().fg(props.theme.fg()),
        ),
    ]));
    lines.push(Line::from(""));

    section_heading(
        &mut lines,
        "CONNECTIONS",
        &format!("{connections_len} saved"),
        props,
    );

    match panel {
        WebPanel::Search => {
            if ws.search_connections.is_empty() {
                empty_connections(
                    &mut lines,
                    "No saved connections. Built-in search presets are still available.",
                    props,
                );
            }
            for (idx, conn) in ws.search_connections.iter().enumerate() {
                let item_index = 2 + idx;
                let active = connection_matches(active_id, &conn.id, conn.preset_id.as_deref());
                let (state, state_ok) = search_connection_state(ws, conn, active);
                push_connection_row(
                    &mut lines,
                    &mut selected_line,
                    item_index,
                    props.detail_index,
                    focused,
                    &conn.display_name(),
                    connection_origin(conn.preset_id.as_deref(), conn.base_url.as_deref()),
                    state,
                    state_ok,
                    &conn.id,
                    props,
                );
            }
        }
        WebPanel::Reader => {
            if ws.reader_connections.is_empty() {
                empty_connections(
                    &mut lines,
                    "No saved readers. Add one to enable rich page extraction.",
                    props,
                );
            }
            for (idx, conn) in ws.reader_connections.iter().enumerate() {
                let item_index = 2 + idx;
                let active = connection_matches(active_id, &conn.id, conn.preset_id.as_deref());
                let state = if !conn.enabled {
                    "Disabled"
                } else if active {
                    "Active"
                } else {
                    "Available"
                };
                push_connection_row(
                    &mut lines,
                    &mut selected_line,
                    item_index,
                    props.detail_index,
                    focused,
                    &conn.display_name(),
                    connection_origin(conn.preset_id.as_deref(), conn.base_url.as_deref()),
                    state,
                    conn.enabled,
                    &conn.id,
                    props,
                );
            }
        }
    }

    let add_index = 2 + connections_len;
    push_action_row(
        &mut lines,
        &mut selected_line,
        add_index,
        props.detail_index,
        focused,
        match panel {
            WebPanel::Search => "+  Add search connection",
            WebPanel::Reader => "+  Add page reader",
        },
        props,
    );

    render_scrollable(
        frame,
        body,
        lines,
        props.detail_scroll,
        selected_line,
        props.theme,
    );
}

fn section_heading(
    lines: &mut Vec<Line<'static>>,
    title: &str,
    note: &str,
    props: &ConfigViewProps<'_>,
) {
    lines.push(Line::from(vec![
        Span::styled(
            title.to_string(),
            Style::default()
                .fg(props.theme.muted())
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(note.to_string(), Style::default().fg(props.theme.dim())),
    ]));
}

#[allow(clippy::too_many_arguments)]
fn push_setting_row(
    lines: &mut Vec<Line<'static>>,
    selected_line: &mut Option<usize>,
    index: usize,
    selected_index: usize,
    focused: bool,
    label: &str,
    value: &str,
    badge: &str,
    help: &str,
    props: &ConfigViewProps<'_>,
) {
    let selected = index == selected_index;
    if selected {
        *selected_line = Some(lines.len());
    }
    lines.push(Line::from(vec![
        cursor_span(selected, props),
        Span::styled(
            format!("{label:<18}"),
            selectable_style(selected, focused, props),
        ),
        Span::styled(
            value.to_string(),
            Style::default()
                .fg(props.theme.fg())
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            badge.to_string(),
            Style::default().fg(if badge == "Active" {
                props.theme.ok()
            } else {
                props.theme.dim()
            }),
        ),
    ]));
    if selected && focused {
        lines.push(help_line(help, props));
    }
}

#[allow(clippy::too_many_arguments)]
fn push_connection_row(
    lines: &mut Vec<Line<'static>>,
    selected_line: &mut Option<usize>,
    index: usize,
    selected_index: usize,
    focused: bool,
    name: &str,
    origin: String,
    state: &str,
    state_ok: bool,
    id: &str,
    props: &ConfigViewProps<'_>,
) {
    let selected = index == selected_index;
    if selected {
        *selected_line = Some(lines.len());
    }
    lines.push(Line::from(vec![
        cursor_span(selected, props),
        Span::styled(
            if state == "Active" { "● " } else { "○ " },
            Style::default().fg(if state == "Active" {
                props.theme.ok()
            } else if state_ok {
                props.theme.dim()
            } else {
                props.theme.warn()
            }),
        ),
        Span::styled(name.to_string(), selectable_style(selected, focused, props)),
        Span::raw("  "),
        Span::styled(
            state.to_string(),
            Style::default().fg(if state == "Active" {
                props.theme.ok()
            } else if state_ok {
                props.theme.muted()
            } else {
                props.theme.warn()
            }),
        ),
    ]));
    lines.push(Line::from(vec![
        Span::raw("       "),
        Span::styled(origin, Style::default().fg(props.theme.dim())),
    ]));
    if selected && focused {
        lines.push(help_line(
            &format!("ID: {id}  ·  Enter to activate  ·  d to delete"),
            props,
        ));
    }
}

fn push_action_row(
    lines: &mut Vec<Line<'static>>,
    selected_line: &mut Option<usize>,
    index: usize,
    selected_index: usize,
    focused: bool,
    label: &str,
    props: &ConfigViewProps<'_>,
) {
    let selected = index == selected_index;
    if selected {
        *selected_line = Some(lines.len());
    }
    lines.push(Line::from(vec![
        cursor_span(selected, props),
        Span::styled(
            label.to_string(),
            if selected && focused {
                Style::default()
                    .fg(props.theme.brand())
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(props.theme.brand())
            },
        ),
    ]));
    if selected && focused {
        lines.push(help_line(
            "Enter to choose a preset or custom endpoint",
            props,
        ));
    }
}

fn empty_connections(lines: &mut Vec<Line<'static>>, text: &str, props: &ConfigViewProps<'_>) {
    lines.push(Line::from(vec![
        Span::raw("     "),
        Span::styled(text.to_string(), Style::default().fg(props.theme.dim())),
    ]));
}

fn cursor_span(selected: bool, props: &ConfigViewProps<'_>) -> Span<'static> {
    Span::styled(
        if selected { " ›  " } else { "    " },
        Style::default().fg(if selected {
            props.theme.brand()
        } else {
            props.theme.dim()
        }),
    )
}

fn selectable_style(selected: bool, focused: bool, props: &ConfigViewProps<'_>) -> Style {
    Style::default()
        .fg(if selected && focused {
            props.theme.brand()
        } else {
            props.theme.fg()
        })
        .add_modifier(Modifier::BOLD)
}

fn help_line(text: &str, props: &ConfigViewProps<'_>) -> Line<'static> {
    Line::from(vec![
        Span::raw("       "),
        Span::styled(text.to_string(), Style::default().fg(props.theme.muted())),
    ])
}

fn connection_matches(active: &str, id: &str, preset: Option<&str>) -> bool {
    active == id || preset == Some(active)
}

fn connection_origin(preset: Option<&str>, base_url: Option<&str>) -> String {
    if let Some(preset) = preset {
        format!("Preset · {preset}")
    } else if let Some(url) = base_url {
        format!("Custom · {url}")
    } else {
        "Custom connection".to_string()
    }
}

fn search_display_name(ws: &muta_contracts::WebSearchConfigView) -> String {
    if ws.provider == "none" {
        return "Disabled".to_string();
    }
    ws.search_connections
        .iter()
        .find(|conn| connection_matches(&ws.provider, &conn.id, conn.preset_id.as_deref()))
        .map(|conn| conn.display_name().to_string())
        .or_else(|| {
            muta_contracts::WebSearchPresets::find(&ws.provider)
                .map(|preset| preset.display_name.to_string())
        })
        .unwrap_or_else(|| ws.provider.clone())
}

fn reader_display_name(ws: &muta_contracts::WebSearchConfigView) -> String {
    if ws.reader == "none" {
        return "Disabled".to_string();
    }
    ws.reader_connections
        .iter()
        .find(|conn| connection_matches(&ws.reader, &conn.id, conn.preset_id.as_deref()))
        .map(|conn| conn.display_name().to_string())
        .or_else(|| {
            muta_contracts::WebReaderPresets::find(&ws.reader)
                .map(|preset| preset.display_name.to_string())
        })
        .unwrap_or_else(|| ws.reader.clone())
}

fn search_connection_state(
    ws: &muta_contracts::WebSearchConfigView,
    conn: &muta_contracts::WebSearchConnection,
    active: bool,
) -> (&'static str, bool) {
    if !conn.enabled {
        return ("Disabled", false);
    }
    let ready = match conn.preset_id.as_deref().unwrap_or(&conn.id) {
        "tavily" => ws.tavily_api_key_set || conn.api_key_env.is_some(),
        "bocha" => ws.bocha_api_key_set || conn.api_key_env.is_some(),
        "searxng" => ws.searxng_url.is_some() || conn.base_url.is_some(),
        _ => true,
    };
    if !ready {
        ("Needs setup", false)
    } else if active {
        ("Active", true)
    } else {
        ("Available", true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn item_counts_track_real_connections() {
        let mut ws =
            muta_contracts::WebSearchConfigView::from(&muta_contracts::WebSearchConfig::default());
        assert_eq!(search_item_count(Some(&ws)), 3);
        assert_eq!(reader_item_count(Some(&ws)), 3);

        ws.search_connections
            .push(muta_contracts::WebSearchConnection::default());
        ws.reader_connections
            .push(muta_contracts::WebReaderConnection::default());
        assert_eq!(search_item_count(Some(&ws)), 4);
        assert_eq!(reader_item_count(Some(&ws)), 4);
    }
}
