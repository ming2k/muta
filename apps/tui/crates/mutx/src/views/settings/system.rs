//! System settings panel: config file paths, environment info, and runtime metrics.

use mutx_engine::{Frame, Line, Modifier, Rect, Span, Style};

use super::{ConfigViewProps, render_scrollable};

pub(super) fn draw_system_detail(
    frame: &mut Frame,
    body: Rect,
    props: &mut ConfigViewProps<'_>,
    _focused: bool,
) {
    let mut lines: Vec<Line<'static>> = Vec::new();

    let items = [
        ("Config File", "~/.config/muta/config.toml"),
        (
            "Web Connections",
            "~/.local/state/muta/web_connections.toml",
        ),
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
                format!("{:<18}", label),
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
