//! Help / keybindings modal.
//!
//! The global-shortcut rows are **not** hard-coded here: they are fed in by
//! the app shell from the unified keybinding registry (`crate::keymap`),
//! which is the single source of truth shared with the input resolver. That
//! way the keys shown in Help can never drift from the keys that actually fire. See [`HelpBinding`] and
//! [`draw_help_modal`].
//!
//! The remaining rows (Enter semantics, line editing, transcript focus, slash
//! commands, modes) are static fallback content for keys that are either
//! context-sensitive (`?`/`ctrl+h` help aliases need an empty prompt / the
//! Kitty protocol), polymorphic (`enter`, `tab`), or non-keyboard (`/tools`,
//! `/pursue`). Those are documented prose, not registry-resolvable bindings.

use neenee_tui_engine::{
    Frame, Modifier, Span, {Line, Style},
};

use crate::components::keycap::keycap_style;
use crate::components::selectable_body::{SelectableRow, render_selectable_body};
use crate::model::layout::LayoutMap;
use crate::model::selection::SelectionState;
use crate::primitives::{
    FixedModalSpec, FooterHint, modal_area, modal_frame, modal_header, render_modal_footer,
};
use crate::view::Theme;

/// One row in the Help modal's "Views & tools" section, projected from the
/// keybinding registry. `key` is the canonical lowercase label (e.g.
/// `ctrl+t`); `description` is the short human text shown beside it.
///
/// This is a plain data type so the view crate can render it without a
/// dependency on the shell — the shell builds the slice from its registry and
/// hands it over each frame.
pub struct HelpBinding {
    pub key: &'static str,
    pub description: &'static str,
}

/// Draw the Help modal. `bindings` is the registry projection for the global
/// shortcuts section; everything else is static fallback prose (see the module
/// docs for why those keys are not registry-resolvable). The body is a
/// selectable document: keycap labels and descriptions can be dragged over
/// and copied like transcript text.
pub fn draw_help_modal(
    frame: &mut Frame,
    scroll: &mut usize,
    bindings: &[HelpBinding],
    theme: &Theme,
    selection: &SelectionState,
    layout_map: &mut LayoutMap,
) -> neenee_tui_engine::Rect {
    let key = |k: &str| Span::styled(format!("{:<10}", k), keycap_style(theme));
    let desc = |d: &str| Span::styled(d.to_string(), Style::default().fg(theme.muted()));
    let section = |title: &str| {
        Span::styled(
            title.to_string(),
            Style::default().fg(theme.fg()).add_modifier(Modifier::BOLD),
        )
    };
    let row = |k: &str, d: &str| Line::from(vec![key(k), desc(d)]);

    let mut body = vec![
        Line::from(section("General")),
        row("enter", "send message"),
        row("alt+enter", "insert newline (ctrl+j)"),
        row("esc", "interrupt (×2) / close"),
    ];

    // ── Global shortcuts (from the keybinding registry) ──
    // These rows are projected from the single source of truth, so Help and
    // the live key handler can never disagree about which global keys exist.
    // `ctrl+c` is deliberately kept here as well so the copy/clear/quit row
    // renders exactly once, sourced from the registry.
    body.extend(bindings.iter().map(|b| row(b.key, b.description)));

    body.extend([
        Line::from(""),
        Line::from(section("While the agent is running")),
        row("enter", "perform the action shown below the prompt"),
        row("tab", "change what Enter will do"),
        row("↑", "with an empty prompt: edit newest waiting message"),
        Line::from(""),
        Line::from(section("Line editing")),
        row("ctrl+a / ctrl+e", "caret to line start / end"),
        row("ctrl+b", "move back one char (←)"),
        row("home / end", "caret to line start / end"),
        row("ctrl+u / ctrl+k", "delete to line start / end"),
        row("ctrl+w", "delete previous word"),
        row("alt+backspace", "delete previous word"),
        row("alt+d", "delete next word"),
        row("ctrl+← / ctrl+→", "move word back / forward"),
        row("alt+b / alt+f", "move word back / forward"),
        Line::from(""),
        Line::from(section("Transcript focus")),
        Line::from(desc(
            "No modes: typing always lands in the prompt. Ctrl+↑/↓ highlights",
        )),
        Line::from(desc(
            "a step; the highlight tells you which keys act on it.",
        )),
        row("ctrl+↑ / ctrl+↓", "focus a step (nearest first)"),
        row("↑ / ↓", "while focused: cycle steps"),
        row("enter", "open the focused step"),
        row("esc", "clear the focus"),
        Line::from(""),
        Line::from(section("Views & tools")),
        // Help aliases that the registry cannot own (context-sensitive /
        // protocol-dependent) are documented here as prose, alongside the
        // slash-command surfaces and the registry-provided globals below them.
        row("? / ctrl+h", "this help (f1 anywhere)"),
        row("/tools", "manage tools"),
        row("/skills", "browse skills"),
        row("/permissions", "manage permissions"),
        row("/settings", "settings & appearance"),
        row("/", "slash commands"),
        Line::from(""),
        Line::from(section("Modes")),
        Line::from(""),
        Line::from(desc("Drag to select; copy with Ctrl+C or Ctrl+Shift+C.")),
    ]);

    // Selectable document body: renders through `render_selectable_body` so
    // every visual row registers a MODAL_DOC region (drag-select + copy).
    // The panel shell (geometry, header, footer) is the same `modal_frame`
    // ceremony other hand-rolled modals use; the document replaces the
    // engine-wrapped `ScrollBody` the `ModalPage` path would have drawn.
    let rows: Vec<SelectableRow> = body.into_iter().map(SelectableRow::from_line).collect();

    let area = modal_area(frame, FixedModalSpec::HELP);
    let f = modal_frame(frame, area, theme.panel(), true, true);

    modal_header(frame, f.header, "Help", theme);
    render_selectable_body(
        frame, f.body, &rows, scroll, None, theme, selection, layout_map,
    );
    if let Some(footer) = f.footer {
        render_modal_footer(
            frame,
            footer,
            &[
                FooterHint::navigation("↑↓", "scroll"),
                FooterHint::always("Esc", "close"),
            ],
            theme,
        );
    }
    area
}
