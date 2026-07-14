//! End-to-end exercise of the headless editor core: typing a sentence,
//! multi-line edits, selection + replace, and undo/redo round-trips. Runs with
//! `--no-default-features` so it needs no GPU or compositor.

use neenee_editor::buffer::{GapBuffer, Offset, Point};
use neenee_editor::editor::{Dir, Editor};
use neenee_editor::selection::Selection;

#[test]
fn type_a_paragraph_and_undo_it() {
    let mut e = Editor::from_text("");
    for word in ["hello", " ", "world", "\n", "second line"] {
        e.move_carets(Dir::LineEnd, false);
        e.insert(word);
    }
    assert_eq!(e.text(), "hello world\nsecond line");
    // Undo clears everything typed (one or more transactions depending on
    // coalescing boundaries). Loop until the undo stack is empty.
    while e.history.can_undo() {
        e.undo();
    }
    assert_eq!(e.text(), "");
}

#[test]
fn select_word_replace_then_redo() {
    let mut e = Editor::from_text("foo bar baz");
    // Select "bar" (bytes 4..7) and replace with "QUX".
    e.selections.all = vec![Selection::new_range(Offset(4), Offset(7))];
    e.insert("QUX");
    assert_eq!(e.text(), "foo QUX baz");
    e.undo();
    assert_eq!(e.text(), "foo bar baz");
    e.redo();
    assert_eq!(e.text(), "foo QUX baz");
}

#[test]
fn multiline_backspace_joins_lines() {
    let mut e = Editor::from_text("line one\nline two");
    // caret at start of "line two" (offset 9)
    e.selections.collapse_to(Offset(9));
    e.backspace();
    assert_eq!(e.text(), "line oneline two");
}

#[test]
fn gap_buffer_handles_large_insert() {
    let mut b = GapBuffer::from_text("");
    let chunk = "abcdefgh".repeat(1000); // 8 KiB
    b.insert(0, &chunk);
    assert_eq!(b.len(), 8000);
    assert_eq!(b.slice(0..8), "abcdefgh");
    assert_eq!(b.slice(7992..8000), "abcdefgh");
    // Point/offset stay correct across the gap.
    let mid = b.point_of_offset(Offset(4000));
    assert_eq!(mid, Point::new(0, 4000));
}

#[test]
fn multi_cursor_typing() {
    let mut e = Editor::from_text("a\na\na");
    // Three carets: one on each line, at col 0 (offsets 0, 2, 4).
    e.selections.all = vec![
        Selection::new_caret(Offset(0)),
        Selection::new_caret(Offset(2)),
        Selection::new_caret(Offset(4)),
    ];
    e.insert("X");
    assert_eq!(e.text(), "Xa\nXa\nXa");
}

#[test]
fn undo_restores_selection_range() {
    let mut e = Editor::from_text("abcd");
    e.selections.all = vec![Selection::new_range(Offset(1), Offset(3))];
    e.insert("Z");
    assert_eq!(e.text(), "aZd");
    e.undo();
    // After undo the caret sits at the start of the restored range.
    assert_eq!(e.text(), "abcd");
    assert_eq!(e.selections.primary().head, Offset(1));
}
