//! Transcript layout-map tests: heights, turn headers, gaps, semantic region recording.

use super::*;

/// The transcript content rect must be recorded after rendering so that
/// clicks on gap rows (which carry no region) still switch keyboard focus
/// to Browse. It must span the horizontal band inside the outer gutters
/// (clicks in the gutters are not transcript clicks) and the vertical
/// extent of drawn content, including the inter-message gap row.
#[test]
fn transcript_content_rect_spans_band_and_gap_rows() {
    let theme = Theme::default();
    let width = 40u16;
    let mut terminal = mutx_engine::TestTerminal::new(width, 24);
    // Two assistant text messages so a `MESSAGE_GAP_ROWS` blank row is
    // emitted between them — that row is rendered but never registered.
    let messages = vec![
        TranscriptMessage::new(muta_contracts::Role::Assistant, "first".to_string()),
        TranscriptMessage::new(muta_contracts::Role::Assistant, "second".to_string()),
    ];
    let mut layout_map = LayoutMap::new();
    terminal.draw(|f| {
        draw_transcript(
            f,
            &mut layout_map,
            TranscriptView {
                messages: &messages,
                scroll: 0,
                selection: &SelectionState::None,
                cell_selection: None,
                backoff_clause: None,
                silent_clause: None,
                pulse_levels: None,
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
                page_hints: None,
                session_head: None,
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

    let rect = layout_map
        .transcript_content_rect()
        .expect("content rect must be recorded when messages are drawn");
    // Horizontal band excludes the outer `TRANSCRIPT_H_INSET` gutters.
    assert_eq!(rect.x, TRANSCRIPT_H_INSET);
    assert_eq!(rect.width, width - 2 * TRANSCRIPT_H_INSET);

    // The whole point of the rect: a gap row between the two messages is
    // rendered but carries no region (clicking it does not resolve to a
    // cursor). It must still fall inside the content rect so the click
    // handler can switch focus to Browse.
    let gap_y = (rect.y..rect.y + rect.height)
        .find(|&y| layout_map.region_at(rect.x, y).is_none())
        .expect("there must be at least one unregistered gap row between the two messages");
    assert!(rect.y <= gap_y && gap_y < rect.y + rect.height);
}
