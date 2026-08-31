//! Web settings panel: orthogonal Search (breadth) and Fetch (depth) segment control and connection routing.

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

            let description = if let Some(preset) =
                conn.preset_id.as_deref().and_then(muta_contracts::WebSearchPresets::find)
            {
                preset.description.to_string()
            } else if let Some(url) = &conn.base_url {
                format!("Endpoint: {url}")
            } else {
                format!("Connection: {}", conn.id)
            };

            let mut item = DropdownItem::new(
                conn.id.clone(),
                conn.display_name(),
                conn.id.clone(),
            )
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
        DropdownItem::new("add_new", "＋ Add Search Connection...", "add_new".to_string())
            .with_description("Declare a new search connection from preset or custom URL")
            .with_indicator(DropdownIndicator::Ready),
    );

    let mut state = DropdownState::new(Some("Select Web Search Connection"), items)
        .with_context("websearch_provider");
    state.select_by_id(current);
    state
}

/// Build a floating dropdown picker for Web Fetch page readers.
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

            let description = if let Some(preset) =
                conn.preset_id.as_deref().and_then(muta_contracts::WebReaderPresets::find)
            {
                preset.description.to_string()
            } else if let Some(url) = &conn.base_url {
                format!("Endpoint: {url}")
            } else {
                format!("Reader: {}", conn.id)
            };

            let mut item = DropdownItem::new(
                conn.id.clone(),
                conn.display_name(),
                conn.id.clone(),
            )
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
            .with_description("Disable web fetch tool")
            .with_indicator(DropdownIndicator::Inactive),
    );

    items.push(
        DropdownItem::new("add_new", "＋ Add Reader Connection...", "add_new".to_string())
            .with_description("Declare a new reader connection from preset or custom URL")
            .with_indicator(DropdownIndicator::Ready),
    );

    let mut state = DropdownState::new(Some("Select Web Fetch Reader"), items)
        .with_context("websearch_reader");
    state.select_by_id(current);
    state
}

/// Build a floating dropdown picker to choose a preset when adding a connection.
pub fn build_add_web_connection_dropdown(segment: usize) -> crate::components::dropdown::DropdownState<String> {
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
            DropdownItem::new("parallel", "Parallel Search (Hosted MCP)", "parallel".to_string())
                .with_description("Hosted MCP Search · Keyless anonymous or API key")
                .with_badge("Search")
                .with_indicator(DropdownIndicator::Ready),
            DropdownItem::new("custom-search", "Custom Search Relay", "custom-search".to_string())
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
            DropdownItem::new("custom-reader", "Custom Web Reader", "custom-reader".to_string())
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

pub(super) fn draw_websearch_detail(
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

    // Segmented Control Tabs (Search vs Reader)
    let is_search_tab = props.web_segment == 0;

    let search_tab_style = if is_search_tab {
        Style::default()
            .bg(props.theme.body())
            .fg(props.theme.brand())
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .bg(props.theme.surface())
            .fg(props.theme.muted())
    };

    let reader_tab_style = if !is_search_tab {
        Style::default()
            .bg(props.theme.body())
            .fg(props.theme.brand())
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .bg(props.theme.surface())
            .fg(props.theme.muted())
    };

    lines.push(Line::from(vec![
        Span::styled("   ", Style::default().bg(props.theme.surface())),
        Span::styled(" [ Search ] ", search_tab_style),
        Span::styled(" ", Style::default().bg(props.theme.surface())),
        Span::styled(" [ Reader ] ", reader_tab_style),
        Span::styled("   ", Style::default().bg(props.theme.surface())),
        Span::styled(
            " [←/→ switch tab]",
            Style::default().fg(props.theme.dim()),
        ),
    ]));
    lines.push(Line::from(""));

    if is_search_tab {
        // ── Search Tab ──────────────────────────────────────────────────────
        let current_search_id = ws.provider.as_str();

        // Item 0: Active Search Connection
        {
            let i = 0;
            let is_sel = i == props.detail_index;
            if is_sel {
                selected_line = Some(lines.len());
            }
            let cursor = if is_sel { "›" } else { " " };
            let row_style = if is_sel && focused {
                Style::default()
                    .fg(props.theme.brand())
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
                    .fg(props.theme.fg())
                    .add_modifier(Modifier::BOLD)
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
                Span::styled("Active Search     ", row_style),
                Span::styled(
                    format!("[ {:<14} ]", current_search_id),
                    Style::default()
                        .fg(props.theme.brand())
                        .add_modifier(Modifier::BOLD),
                ),
            ]));
            if is_sel && focused {
                lines.push(Line::from(vec![
                    Span::raw("     "),
                    Span::styled(
                        "Select active search connection [Enter to choose]",
                        Style::default().fg(props.theme.muted()),
                    ),
                ]));
            }
            lines.push(Line::from(""));
        }

        // Item 1: Timeout
        {
            let i = 1;
            let is_sel = i == props.detail_index;
            if is_sel {
                selected_line = Some(lines.len());
            }
            let cursor = if is_sel { "›" } else { " " };
            let row_style = if is_sel && focused {
                Style::default()
                    .fg(props.theme.brand())
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
                    .fg(props.theme.fg())
                    .add_modifier(Modifier::BOLD)
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
                Span::styled("Request Timeout   ", row_style),
                Span::styled(
                    format!("[ {:>2} s ]", ws.timeout_secs),
                    Style::default()
                        .fg(props.theme.brand())
                        .add_modifier(Modifier::BOLD),
                ),
            ]));
            if is_sel && focused {
                lines.push(Line::from(vec![
                    Span::raw("     "),
                    Span::styled(
                        "[Enter to +5s (min 5s, max 120s)]",
                        Style::default().fg(props.theme.dim()),
                    ),
                ]));
            }
            lines.push(Line::from(""));
        }

        // Item 2: Add Search Connection Action
        {
            let i = 2;
            let is_sel = i == props.detail_index;
            if is_sel {
                selected_line = Some(lines.len());
            }
            let cursor = if is_sel { "›" } else { " " };
            let row_style = if is_sel && focused {
                Style::default()
                    .fg(props.theme.brand())
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(props.theme.brand())
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
                Span::styled("［ ＋ Add Search Connection ］", row_style),
            ]));
            if is_sel && focused {
                lines.push(Line::from(vec![
                    Span::raw("     "),
                    Span::styled(
                        "[Enter to open search preset chooser]",
                        Style::default().fg(props.theme.dim()),
                    ),
                ]));
            }
            lines.push(Line::from(""));
        }

        // Configured Search Instances Table
        lines.push(Line::from(Span::styled(
            "Configured Instances:",
            Style::default()
                .fg(props.theme.muted())
                .add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(""));

        for (idx, conn) in ws.search_connections.iter().enumerate() {
            let row_idx = 3 + idx;
            let is_sel = row_idx == props.detail_index;
            if is_sel {
                selected_line = Some(lines.len());
            }
            let cursor = if is_sel { "›" } else { " " };

            let is_active = ws.provider == conn.id
                || ws.provider == conn.preset_id.as_deref().unwrap_or("");
            let status_mark = if is_active { "●" } else { "○" };
            let tag = if is_active { " [Active]" } else { "" };

            let endpoint_info = conn
                .base_url
                .as_deref()
                .unwrap_or_else(|| conn.preset_id.as_deref().unwrap_or("preset"));

            let name_style = if is_sel && focused {
                Style::default()
                    .fg(props.theme.brand())
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
                    .fg(props.theme.fg())
                    .add_modifier(Modifier::BOLD)
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
                    format!("{status_mark} "),
                    Style::default().fg(if is_active {
                        props.theme.ok()
                    } else {
                        props.theme.dim()
                    }),
                ),
                Span::styled(format!("{:<20}", conn.display_name()), name_style),
                Span::styled(
                    format!("{:<30}", endpoint_info),
                    Style::default().fg(props.theme.dim()),
                ),
                Span::styled(tag, Style::default().fg(props.theme.brand())),
            ]));
            if is_sel && focused {
                lines.push(Line::from(vec![
                    Span::raw("     "),
                    Span::styled(
                        format!("ID: {}  [Enter: activate]  [d: delete instance]", conn.id),
                        Style::default().fg(props.theme.muted()),
                    ),
                ]));
            }
        }
    } else {
        // ── Fetch Tab ───────────────────────────────────────────────────────
        let current_reader_id = ws.reader.as_str();

        // Item 0: Active Reader Connection
        {
            let i = 0;
            let is_sel = i == props.detail_index;
            if is_sel {
                selected_line = Some(lines.len());
            }
            let cursor = if is_sel { "›" } else { " " };
            let row_style = if is_sel && focused {
                Style::default()
                    .fg(props.theme.brand())
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
                    .fg(props.theme.fg())
                    .add_modifier(Modifier::BOLD)
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
                Span::styled("Active Reader     ", row_style),
                Span::styled(
                    format!("[ {:<14} ]", current_reader_id),
                    Style::default()
                        .fg(props.theme.brand())
                        .add_modifier(Modifier::BOLD),
                ),
            ]));
            if is_sel && focused {
                lines.push(Line::from(vec![
                    Span::raw("     "),
                    Span::styled(
                        "Select active reader connection [Enter to choose]",
                        Style::default().fg(props.theme.muted()),
                    ),
                ]));
            }
            lines.push(Line::from(""));
        }

        // Item 1: Timeout
        {
            let i = 1;
            let is_sel = i == props.detail_index;
            if is_sel {
                selected_line = Some(lines.len());
            }
            let cursor = if is_sel { "›" } else { " " };
            let row_style = if is_sel && focused {
                Style::default()
                    .fg(props.theme.brand())
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
                    .fg(props.theme.fg())
                    .add_modifier(Modifier::BOLD)
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
                Span::styled("Request Timeout   ", row_style),
                Span::styled(
                    format!("[ {:>2} s ]", ws.timeout_secs),
                    Style::default()
                        .fg(props.theme.brand())
                        .add_modifier(Modifier::BOLD),
                ),
            ]));
            if is_sel && focused {
                lines.push(Line::from(vec![
                    Span::raw("     "),
                    Span::styled(
                        "[Enter to +5s (min 5s, max 120s)]",
                        Style::default().fg(props.theme.dim()),
                    ),
                ]));
            }
            lines.push(Line::from(""));
        }

        // Item 2: Add Reader Connection Action
        {
            let i = 2;
            let is_sel = i == props.detail_index;
            if is_sel {
                selected_line = Some(lines.len());
            }
            let cursor = if is_sel { "›" } else { " " };
            let row_style = if is_sel && focused {
                Style::default()
                    .fg(props.theme.brand())
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(props.theme.brand())
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
                Span::styled("［ ＋ Add Reader Connection ］", row_style),
            ]));
            if is_sel && focused {
                lines.push(Line::from(vec![
                    Span::raw("     "),
                    Span::styled(
                        "[Enter to open reader preset chooser]",
                        Style::default().fg(props.theme.dim()),
                    ),
                ]));
            }
            lines.push(Line::from(""));
        }

        // Configured Reader Instances Table
        lines.push(Line::from(Span::styled(
            "Configured Instances:",
            Style::default()
                .fg(props.theme.muted())
                .add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(""));

        if ws.reader_connections.is_empty() {
            lines.push(Line::from(vec![
                Span::raw("   "),
                Span::styled(
                    "○ (No reader instances configured — reader is disabled)",
                    Style::default().fg(props.theme.dim()),
                ),
            ]));
        } else {
            for (idx, conn) in ws.reader_connections.iter().enumerate() {
                let row_idx = 3 + idx;
                let is_sel = row_idx == props.detail_index;
                if is_sel {
                    selected_line = Some(lines.len());
                }
                let cursor = if is_sel { "›" } else { " " };

                let is_active = ws.reader == conn.id
                    || ws.reader == conn.preset_id.as_deref().unwrap_or("");
                let status_mark = if is_active { "●" } else { "○" };
                let tag = if is_active { " [Active]" } else { "" };

                let endpoint_info = conn
                    .base_url
                    .as_deref()
                    .unwrap_or_else(|| conn.preset_id.as_deref().unwrap_or("preset"));

                let name_style = if is_sel && focused {
                    Style::default()
                        .fg(props.theme.brand())
                        .add_modifier(Modifier::BOLD)
                    } else {
                    Style::default()
                        .fg(props.theme.fg())
                        .add_modifier(Modifier::BOLD)
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
                        format!("{status_mark} "),
                        Style::default().fg(if is_active {
                            props.theme.ok()
                        } else {
                            props.theme.dim()
                        }),
                    ),
                    Span::styled(format!("{:<20}", conn.display_name()), name_style),
                    Span::styled(
                        format!("{:<30}", endpoint_info),
                        Style::default().fg(props.theme.dim()),
                    ),
                    Span::styled(tag, Style::default().fg(props.theme.brand())),
                ]));
                if is_sel && focused {
                    lines.push(Line::from(vec![
                        Span::raw("     "),
                        Span::styled(
                            format!("ID: {}  [Enter: activate]  [d: delete instance]", conn.id),
                            Style::default().fg(props.theme.muted()),
                        ),
                    ]));
                }
            }
        }
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
