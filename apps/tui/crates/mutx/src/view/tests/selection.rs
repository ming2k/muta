//! Semantic selection tests: wrapped-line intersection, block coverage, virtual index geometry.

use super::*;

#[test]
fn virtual_index_selects_only_chunks_intersecting_the_viewport() {
    let messages = (0..4)
        .map(|i| TranscriptMessage::new(muta_contracts::Role::Assistant, format!("m{i}")))
        .collect::<Vec<_>>();
    let mut cache = HeightCache::default();
    cache.prepare(80);
    // Four-line bodies plus one boundary row owned by each following
    // message: chunks begin at 0, 4, 9, and 14.
    for message in &messages {
        cache.set(message.id, 4);
    }

    let window = cache
        .virtual_window(&messages, crate::layout::Strategy::TurnBand, 6, 3)
        .expect("all message heights are cached");
    assert_eq!(window.message_start, 1);
    assert_eq!(window.message_end, 2);
    assert_eq!(window.prefix_lines, 4);
    assert_eq!(window.skip_rows, 2);
    assert_eq!(window.total_lines, 19);
}

#[test]
fn virtual_index_uses_segmented_same_turn_geometry() {
    let mut thinking = TranscriptMessage::thinking("reasoning").with_turn(3);
    thinking.set_thinking_duration(1);
    let first = TranscriptMessage::tool_step("a", "read_text", r#"{"path":"a"}"#).with_turn(3);
    let second = TranscriptMessage::tool_step("b", "read_text", r#"{"path":"b"}"#).with_turn(3);
    let messages = vec![thinking, first, second];
    let mut cache = HeightCache::default();
    cache.prepare(80);
    for message in &messages {
        cache.set(message.id, 2);
    }

    let window = cache
        .virtual_window(&messages, crate::layout::Strategy::TurnBand, 0, 20)
        .expect("all message heights are cached");
    assert_eq!(window.message_start, 0);
    assert_eq!(window.message_end, 3);
    assert_eq!(
        window.total_lines, 9,
        "header + header gap + thinking + segment gap + flush tool batch"
    );
}

#[test]
fn line_selection_intersects_wrapped_lines() {
    use crate::model::layout::SemanticCursor;
    let sel = SelectionState::Range {
        anchor: SemanticCursor::new(0, 0, 2),
        head: SemanticCursor::new(0, 0, 8),
    };
    let range = block_selection_range(&sel, 0, 0);

    // Line covering bytes 0..5 ("hello"): selected from 2 to end.
    let first = WrappedLine {
        text: "hello".to_string(),
        start_byte: 0,
        end_byte: 5,
    };
    assert_eq!(line_selection(range, &first), Some((2, 5)));

    // Line covering bytes 5..10 ("world"): selected up to head char (8 → rel 3, inclusive → 4).
    let second = WrappedLine {
        text: "world".to_string(),
        start_byte: 5,
        end_byte: 10,
    };
    assert_eq!(line_selection(range, &second), Some((0, 4)));

    // A line after the selection has no overlap.
    let third = WrappedLine {
        text: "after".to_string(),
        start_byte: 10,
        end_byte: 15,
    };
    assert_eq!(line_selection(range, &third), None);
}

#[test]
fn block_selection_covers_middle_blocks_fully() {
    use crate::model::layout::SemanticCursor;
    let sel = SelectionState::Range {
        anchor: SemanticCursor::new(0, 0, 3),
        head: SemanticCursor::new(0, 2, 1),
    };
    assert_eq!(block_selection_range(&sel, 0, 0), Some((3, None)));
    assert_eq!(block_selection_range(&sel, 0, 1), Some((0, None)));
    assert_eq!(block_selection_range(&sel, 0, 2), Some((0, Some(1))));
    assert_eq!(block_selection_range(&sel, 0, 3), None);
    assert_eq!(block_selection_range(&sel, 1, 0), None);
}
