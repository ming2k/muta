//! Authoritative Help Modal (F1).
//!
//! Fully derived from the Action & Command Registry SSOT (`crate::keymap`).
//! Groups commands into Global, Contextual/Session, Navigation, and Management,
//! rendering accurate descriptions and key combinations without static drift.

use mutx_engine::{
    Frame, Modifier, Rect, Span, {Line, Style},
};

use crate::components::keycap::keycap_style;
use crate::components::selectable_body::{SelectableRow, render_selectable_body};
use crate::keymap::{
    AppContext, Availability, COMMAND_REGISTRY, CommandCategory, Scope,
};
use crate::model::layout::LayoutMap;
use crate::model::selection::SelectionState;
use crate::primitives::{
    FixedModalSpec, FooterHint, modal_area, modal_frame, modal_header, render_modal_footer,
};
use crate::view::Theme;

/// Draw the dynamic Help modal derived from `COMMAND_REGISTRY`.
pub fn draw_help_modal(
    frame: &mut Frame,
    scroll: &mut usize,
    ctx: &AppContext,
    theme: &Theme,
    selection: &SelectionState,
    layout_map: &mut LayoutMap,
) -> Rect {
    let spec = FixedModalSpec::HELP;
    let outer_rect = modal_area(frame, spec);
    let f = modal_frame(frame, outer_rect, theme.panel(), true, true);

    modal_header(frame, f.header, "Help & Key Reference (F1)", theme);

    let key_fmt = |k: &str| Span::styled(format!("{:<16}", k), keycap_style(theme));
    let desc_fmt = |d: &str| Span::styled(d.to_string(), theme.keycap_label_style());
    let section_fmt = |title: &str| {
        Span::styled(
            title.to_string(),
            Style::default().fg(theme.brand()).add_modifier(Modifier::BOLD),
        )
    };
    let row_fmt = |k: &str, d: &str| Line::from(vec![key_fmt(k), desc_fmt(d)]);

    let mut rows: Vec<SelectableRow> = Vec::new();

    // ── 1. Global Core Shortcuts (6 Canonical Keys) ──
    rows.push(SelectableRow::from_line(Line::from(section_fmt("Global Core Keys"))));
    for cmd in COMMAND_REGISTRY.iter().filter(|c| c.scope == Scope::Global && c.category == CommandCategory::Global) {
        let key_str = if !cmd.bindings.is_empty() {
            cmd.bindings[0].display()
        } else {
            cmd.hint
        };
        rows.push(SelectableRow::from_line(row_fmt(key_str, cmd.description)));
    }

    // ── 2. Current Context / Session Controls ──
    rows.push(SelectableRow::from_line(Line::from("")));
    rows.push(SelectableRow::from_line(Line::from(section_fmt("Session & Focus Controls"))));
    for cmd in COMMAND_REGISTRY.iter().filter(|c| c.scope == Scope::Session || c.scope == Scope::Composer || c.scope == Scope::Transcript) {
        let key_str = if !cmd.bindings.is_empty() {
            cmd.bindings[0].display()
        } else {
            cmd.hint
        };
        let desc = match (cmd.availability)(ctx) {
            Availability::Available => cmd.description.to_string(),
            Availability::Unavailable(reason) => format!("{} ({})", cmd.description, reason),
        };
        rows.push(SelectableRow::from_line(row_fmt(key_str, &desc)));
    }

    // ── 3. Readline Text Editing Reference ──
    rows.push(SelectableRow::from_line(Line::from("")));
    rows.push(SelectableRow::from_line(Line::from(section_fmt("Composer Line Editing"))));
    rows.push(SelectableRow::from_line(row_fmt("Ctrl+A / Home", "Move cursor to line start")));
    rows.push(SelectableRow::from_line(row_fmt("Ctrl+E / End", "Move cursor to line end")));
    rows.push(SelectableRow::from_line(row_fmt("Ctrl+U", "Clear prompt line from cursor to start")));
    rows.push(SelectableRow::from_line(row_fmt("Ctrl+K", "Clear prompt line from cursor to end")));
    rows.push(SelectableRow::from_line(row_fmt("Ctrl+W / Alt+Bksp", "Delete word before cursor")));
    rows.push(SelectableRow::from_line(row_fmt("Alt+D", "Delete word after cursor")));
    rows.push(SelectableRow::from_line(row_fmt("Alt+B / Alt+F", "Move cursor backward / forward word")));
    rows.push(SelectableRow::from_line(row_fmt("Ctrl+V", "Paste clipboard text or image")));

    // ── 4. Surface Navigation & Discovery ──
    rows.push(SelectableRow::from_line(Line::from("")));
    rows.push(SelectableRow::from_line(Line::from(section_fmt("Navigation (Open via Ctrl+L or Slash)"))));
    for cmd in COMMAND_REGISTRY.iter().filter(|c| c.category == CommandCategory::Navigate || c.category == CommandCategory::Settings) {
        let trigger = cmd.slash.unwrap_or(cmd.hint);
        rows.push(SelectableRow::from_line(row_fmt(trigger, cmd.description)));
    }

    render_selectable_body(frame, f.body, &rows, scroll, None, theme, selection, layout_map);

    if let Some(fo) = f.footer {
        let footer_hints = [
            FooterHint::navigation("↑↓", "scroll"),
            FooterHint::always("Esc", "close"),
        ];
        render_modal_footer(frame, fo, &footer_hints, theme);
    }

    outer_rect
}
