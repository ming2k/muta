//! The view rendering test suite, split by surface. Shared fixtures
//! (full-view renderer, grid row reader) live here.

use super::*;
use crate::composer::{ComposerText, ComposerView};
use crate::markdown_table::{build_table_render, shrink_column_widths};
use crate::text_layout::wrap_text;
use unicode_width::UnicodeWidthStr;

fn render_full_view(
    width: u16,
    height: u16,
    messages: &[TranscriptMessage],
    page_hints: Option<PageHints<'_>>,
) -> mutx_engine::TestTerminal {
    let theme = Theme::default();
    let mut terminal = mutx_engine::TestTerminal::new(width, height);
    let hints = page_hints;
    terminal.draw(|f| {
        let _ = draw_transcript(
            f,
            &mut LayoutMap::new(),
            TranscriptView {
                messages,
                scroll: 0,
                selection: &SelectionState::None,
                cell_selection: None,
                backoff_clause: None,
                silent_clause: None,
                activity: "",
                awaiting_permission: false,
                spinner_phase: 0,
                input: "",
                byte_cursor: 0,
                chrome_hidden: false,
                queue_bar: QueueBarView {
                    items: &[],
                    paused: false,
                    blocked: false,
                },
                runner_bar: None,
                side_banner: None,
                page_hints: hints,
                session_head: Some(SessionHead {
                    session_id: "sess-01a2b3c4",
                    workspace: "~/projects/xx",
                    delegated: false,
                }),
                todos: None,
                round_started_at: None,
                hovered_step: None,
                focused_target: None,
                logo: None,
                guidance: EmptyStateGuidance::Tour,
                carousel_index: 0,
                theme: &theme,
                layout: crate::layout::Strategy::default(),
                height_cache: None,
            },
        );
    });
    terminal
}

fn grid_row(terminal: &mutx_engine::TestTerminal, y: u16) -> String {
    let buffer = terminal.buffer();
    let width = buffer.area().width;
    (0..width).map(|x| buffer[(x, y)].symbol()).collect()
}

mod chrome;
mod composer;
mod history_panel;
mod layout_map;
mod selection;
mod tables;
mod tool_steps;

#[test]
fn transcript_view_modular_props_extraction() {
    let theme = Theme::default();
    let view = TranscriptView {
        messages: &[],
        scroll: 5,
        selection: &SelectionState::None,
        cell_selection: None,
        backoff_clause: Some("retry"),
        silent_clause: None,
        activity: "running",
        awaiting_permission: true,
        spinner_phase: 1,
        input: "hello",
        byte_cursor: 3,
        chrome_hidden: false,
        queue_bar: QueueBarView {
            items: &[],
            paused: false,
            blocked: false,
        },
        runner_bar: None,
        side_banner: None,
        page_hints: None,
        session_head: Some(SessionHead {
            session_id: "sess-12345678",
            workspace: "~/proj",
            delegated: true,
        }),
        todos: None,
        round_started_at: None,
        hovered_step: Some(2),
        focused_target: None,
        logo: None,
        guidance: EmptyStateGuidance::Tour,
        carousel_index: 0,
        theme: &theme,
        layout: crate::layout::Strategy::default(),
        height_cache: None,
    };

    let stream = view.stream_props();
    assert_eq!(stream.scroll, 5);
    assert_eq!(stream.session_head.unwrap().session_id, "sess-12345678");

    let chrome = view.chrome_props();
    assert_eq!(chrome.activity, "running");
    assert_eq!(chrome.backoff_clause, Some("retry"));
    assert!(chrome.awaiting_permission);

    let input = view.input_props();
    assert_eq!(input.input, "hello");
    assert_eq!(input.byte_cursor, 3);

    let interaction = view.interaction_props();
    assert_eq!(interaction.hovered_step, Some(2));
}
