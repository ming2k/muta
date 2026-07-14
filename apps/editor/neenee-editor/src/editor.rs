//! The controller: holds buffer + selections + history and turns commands into
//! edits. Movement is grapheme-aware and word-aware; vertical motion remembers
//! a preferred column so repeated up/down keep their horizontal aim. This is
//! the seam `main.rs`'s keymap drives.

use crate::buffer::{GapBuffer, Offset, Point};
use crate::history::{Edit, History, KEY_BACKSPACE, KEY_DELETE, KEY_TYPE};
use crate::selection::{Selection, Selections};

/// A discrete movement direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    Left,
    Right,
    Up,
    Down,
    WordLeft,
    WordRight,
    LineStart,
    LineEnd,
    DocStart,
    DocEnd,
}

/// The editor core. The view (`render.rs`) borrows it read-only each frame.
pub struct Editor {
    pub buffer: GapBuffer,
    pub selections: Selections,
    pub history: History,
    /// Desired column for vertical motion (preserved across up/down).
    preferred_col: Option<u32>,
    /// Set whenever buffer/selections change and a repaint is due.
    pub dirty: bool,
}

impl Default for Editor {
    fn default() -> Self {
        Self::new()
    }
}

impl Editor {
    pub fn new() -> Self {
        Self {
            buffer: GapBuffer::new(),
            selections: Selections::new(Offset(0)),
            history: History::default(),
            preferred_col: None,
            dirty: true,
        }
    }

    pub fn from_text(s: &str) -> Self {
        let mut e = Self::new();
        e.buffer.replace_all(s);
        e.selections.collapse_to(Offset(0));
        e.history.clear();
        e.dirty = true;
        e
    }

    pub fn text(&self) -> String {
        self.buffer.text()
    }

    pub fn len(&self) -> usize {
        self.buffer.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }

    /// Convenience for the status bar: offset → point.
    pub fn point_of_offset(&mut self, o: Offset) -> Point {
        self.buffer.point_of_offset(o)
    }

    // ---- movement -------------------------------------------------------

    /// Move every selection's head in `dir`. With `extend`, keep the anchor
    /// (selection grows); without, collapse to a caret at the new head.
    pub fn move_carets(&mut self, dir: Dir, extend: bool) {
        // Snapshot current heads, then compute new heads with exclusive borrows.
        let heads: Vec<Offset> = self.selections.all.iter().map(|s| s.head).collect();
        let mut new_heads = Vec::with_capacity(heads.len());
        for h in heads {
            new_heads.push(self.move_offset(h, dir));
        }
        for (sel, nh) in self.selections.all.iter_mut().zip(new_heads) {
            if extend {
                sel.head = nh;
            } else {
                sel.anchor = nh;
                sel.head = nh;
            }
        }
        self.selections.all.sort_by_key(|s| s.start());
        self.dirty = true;
    }

    fn move_offset(&mut self, from: Offset, dir: Dir) -> Offset {
        let len = self.buffer.len();
        match dir {
            Dir::Left => self.buffer.prev_grapheme(from),
            Dir::Right => self.buffer.next_grapheme(from),
            Dir::WordLeft => self.prev_word(from),
            Dir::WordRight => self.next_word(from),
            Dir::LineStart => {
                let p = self.buffer.point_of_offset(from);
                self.buffer.offset_of_point(Point::new(p.row, 0))
            }
            Dir::LineEnd => {
                let p = self.buffer.point_of_offset(from);
                self.row_end_offset(p.row)
            }
            Dir::DocStart => {
                self.preferred_col = None;
                Offset(0)
            }
            Dir::DocEnd => {
                self.preferred_col = None;
                Offset(len)
            }
            Dir::Up | Dir::Down => {
                let p = self.buffer.point_of_offset(from);
                let target_col = self.preferred_col.unwrap_or(p.column);
                let rc = self.buffer.row_count();
                let target_row = if dir == Dir::Up {
                    p.row.saturating_sub(1)
                } else {
                    p.row + 1
                };
                let clamped = target_row.min(rc.saturating_sub(1));
                let o = self.buffer.offset_of_point(Point::new(clamped, target_col));
                self.preferred_col = Some(target_col);
                o
            }
        }
    }

    fn row_end_offset(&mut self, row: u32) -> Offset {
        let rc = self.buffer.row_count();
        if row + 1 >= rc {
            Offset(self.buffer.len())
        } else {
            // start of next row, minus the '\n'
            let next = self.buffer.offset_of_point(Point::new(row + 1, 0));
            Offset(next.0.saturating_sub(1))
        }
    }

    fn next_word(&self, o: Offset) -> Offset {
        let s = self.buffer.text();
        let bytes = s.as_bytes();
        let is_word = |b: u8| (b as char).is_alphanumeric();
        let mut i = o.0.min(s.len());
        let started_word = i < bytes.len() && is_word(bytes[i]);
        while i < bytes.len() {
            let cur_word = is_word(bytes[i]);
            if started_word && !cur_word {
                break;
            }
            if !started_word && cur_word {
                break;
            }
            i = next_char_boundary(&s, i);
        }
        // Then skip trailing whitespace (not newlines — those stop a word move).
        while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') {
            i = next_char_boundary(&s, i);
        }
        Offset(i.min(s.len()))
    }

    fn prev_word(&self, o: Offset) -> Offset {
        let s = self.buffer.text();
        let bytes = s.as_bytes();
        let is_word = |b: u8| (b as char).is_alphanumeric();
        let mut i = o.0.min(s.len());
        // Skip whitespace backwards.
        while i > 0 {
            let prev = prev_char_boundary(&s, i);
            let b = bytes[prev];
            if b == b' ' || b == b'\t' || b == b'\n' {
                i = prev;
            } else {
                break;
            }
        }
        // Skip the word backwards.
        let start_word = i > 0 && is_word(bytes[prev_char_boundary(&s, i)]);
        while i > 0 {
            let prev = prev_char_boundary(&s, i);
            if is_word(bytes[prev]) == start_word && start_word {
                i = prev;
            } else if start_word {
                break;
            } else {
                // not a word char (punctuation): step back one
                i = prev;
                break;
            }
        }
        Offset(i)
    }

    // ---- editing --------------------------------------------------------

    /// Insert `s` at every selection, replacing any non-caret ranges first.
    pub fn insert(&mut self, s: &str) {
        self.history.begin(KEY_TYPE);
        self.edit_selections(|buf, sel| {
            let (lo, hi) = (sel.start().0, sel.end().0);
            let deleted = buf.slice(lo..hi);
            buf.delete(lo..hi);
            buf.insert(lo, s);
            Some(
                Edit {
                    delete: Offset(lo)..Offset(hi),
                    insert: s.to_string(),
                    deleted_text: String::new(),
                }
                .with_inverse_text(deleted),
            )
        });
        self.commit_and_reposition();
    }

    pub fn backspace(&mut self) {
        self.history.begin(KEY_BACKSPACE);
        self.edit_selections(|buf, sel| {
            let (lo, hi) = (sel.start().0, sel.end().0);
            if lo == hi {
                let prev = buf.prev_grapheme(Offset(lo));
                if prev.0 < lo {
                    let text = buf.slice(prev.0..lo);
                    buf.delete(prev.0..lo);
                    return Some(
                        Edit {
                            delete: Offset(prev.0)..Offset(lo),
                            insert: String::new(),
                            deleted_text: String::new(),
                        }
                        .with_inverse_text(text),
                    );
                }
                return None;
            }
            let text = buf.slice(lo..hi);
            buf.delete(lo..hi);
            Some(
                Edit {
                    delete: Offset(lo)..Offset(hi),
                    insert: String::new(),
                    deleted_text: String::new(),
                }
                .with_inverse_text(text),
            )
        });
        self.commit_and_reposition();
    }

    pub fn delete(&mut self) {
        self.history.begin(KEY_DELETE);
        self.edit_selections(|buf, sel| {
            let (lo, hi) = (sel.start().0, sel.end().0);
            if lo == hi {
                let next = buf.next_grapheme(Offset(lo));
                if next.0 > lo {
                    let text = buf.slice(lo..next.0);
                    buf.delete(lo..next.0);
                    return Some(
                        Edit {
                            delete: Offset(lo)..Offset(next.0),
                            insert: String::new(),
                            deleted_text: String::new(),
                        }
                        .with_inverse_text(text),
                    );
                }
                return None;
            }
            let text = buf.slice(lo..hi);
            buf.delete(lo..hi);
            Some(
                Edit {
                    delete: Offset(lo)..Offset(hi),
                    insert: String::new(),
                    deleted_text: String::new(),
                }
                .with_inverse_text(text),
            )
        });
        self.commit_and_reposition();
    }

    /// Apply `f` to each selection from last to first so earlier offsets stay
    /// valid as we mutate. Selections become carets at the end of each insert.
    fn edit_selections<F>(&mut self, mut f: F)
    where
        F: FnMut(&mut GapBuffer, Selection<Offset>) -> Option<Edit>,
    {
        self.selections.all.sort_by_key(|s| s.start());
        let n = self.selections.all.len();
        let mut edits: Vec<(usize, Edit)> = Vec::with_capacity(n);
        for idx in (0..n).rev() {
            let sel = self.selections.all[idx];
            if let Some(e) = f(&mut self.buffer, sel) {
                let new_off = Offset(e.delete.start.0 + e.insert.len());
                self.selections.all[idx] = Selection::new_caret(new_off);
                edits.push((idx, e));
            }
        }
        edits.sort_by_key(|(i, _)| *i);
        for (_, e) in edits {
            self.history.record(e);
        }
    }

    fn commit_and_reposition(&mut self) {
        self.history.commit();
        self.selections.all.sort_by_key(|s| s.start());
        self.preferred_col = None;
        self.dirty = true;
    }

    pub fn undo(&mut self) {
        if let Some(range) = self.history.undo(&mut self.buffer) {
            self.selections.collapse_to(range.start);
        }
        self.dirty = true;
    }

    pub fn redo(&mut self) {
        if let Some(range) = self.history.redo(&mut self.buffer) {
            self.selections.collapse_to(range.start);
        }
        self.dirty = true;
    }

    /// Select-all: one selection over the whole buffer.
    pub fn select_all(&mut self) {
        let end = Offset(self.buffer.len());
        self.selections.all = vec![Selection::new_range(Offset(0), end)];
        self.dirty = true;
    }

    /// Click: collapse to `at` (or extend the primary selection if `extend`).
    pub fn click(&mut self, at: Offset, extend: bool) {
        if extend {
            self.selections.primary_mut().head = at;
        } else {
            self.selections.collapse_to(at);
        }
        self.preferred_col = None;
        self.dirty = true;
    }

    /// Add a caret at `at` (cmd/ctrl-click multi-cursor).
    pub fn add_caret(&mut self, at: Offset) {
        self.selections.add_caret(at);
        self.dirty = true;
    }
}

// ---- helpers -------------------------------------------------------------

/// Set the [`Edit::deleted_text`] (the text the edit removed) for undo.
/// Kept as a named builder so call sites read as "record an edit, attaching
/// the text it deleted".
trait EditExt {
    fn with_inverse_text(self, text: String) -> Edit;
}
impl EditExt for Edit {
    fn with_inverse_text(self, text: String) -> Edit {
        Edit {
            deleted_text: text,
            ..self
        }
    }
}

fn next_char_boundary(s: &str, i: usize) -> usize {
    let mut j = i + 1;
    while j < s.len() && !s.is_char_boundary(j) {
        j += 1;
    }
    j
}

fn prev_char_boundary(s: &str, i: usize) -> usize {
    let mut j = i;
    while j > 0 && !s.is_char_boundary(j) {
        j -= 1;
    }
    j
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type_and_undo() {
        let mut e = Editor::from_text("hi");
        // from_text leaves the caret at offset 0; move to end then insert.
        e.move_carets(Dir::LineEnd, false);
        e.insert("!");
        assert_eq!(e.text(), "hi!");
        e.undo();
        assert_eq!(e.text(), "hi");
    }

    #[test]
    fn movement_clamps_at_edges() {
        let mut e = Editor::from_text("abc");
        e.selections.collapse_to(Offset(0));
        e.move_carets(Dir::Left, false);
        assert_eq!(e.selections.primary().head, Offset(0));
        e.move_carets(Dir::Right, false);
        e.move_carets(Dir::Right, false);
        e.move_carets(Dir::Right, false);
        e.move_carets(Dir::Right, false);
        assert_eq!(e.selections.primary().head, Offset(3));
    }

    #[test]
    fn vertical_movement_keeps_column() {
        let mut e = Editor::from_text("aaaa\nbbbb\ncccc");
        // offset 6 = second 'b' on row 1 (col 1). Up keeps col, down keeps col.
        e.selections.collapse_to(Offset(6));
        e.move_carets(Dir::Up, false);
        assert_eq!(
            e.point_of_offset(e.selections.primary().head),
            Point::new(0, 1)
        );
        e.move_carets(Dir::Down, false);
        e.move_carets(Dir::Down, false);
        assert_eq!(
            e.point_of_offset(e.selections.primary().head),
            Point::new(2, 1)
        );
    }

    #[test]
    fn select_all_then_replace() {
        let mut e = Editor::from_text("hello");
        e.select_all();
        e.insert("bye");
        assert_eq!(e.text(), "bye");
    }

    #[test]
    fn backspace_merges_into_line() {
        let mut e = Editor::from_text("ab\ncd");
        e.selections.collapse_to(Offset(3)); // start of row 1
        e.backspace(); // deletes the '\n'
        assert_eq!(e.text(), "abcd");
    }
}
