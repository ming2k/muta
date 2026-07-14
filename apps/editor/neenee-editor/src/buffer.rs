//! Line-aware UTF-8 text store (rope-lite).
//!
//! Modelled on Zed's `text::Buffer` but drastically simpler: a single gap
//! buffer with a lazily-rebuilt line-start cache. Exposes [`Offset`] (byte
//! offset) and [`Point`] (row, column) plus grapheme-aware walks. Positions are
//! *grapheme boundaries*; editing never splits a grapheme cluster.
//!
//! A gap buffer is `O(move + edit)` at the caret and `O(n)` for a line-start
//! cache rebuild on edit — good enough for files into the low-MB range. The
//! public API mirrors a rope so a future `SumTree`-backed implementation is a
//! localized swap.

use std::ops::Range;

use unicode_segmentation::GraphemeCursor;

/// A byte offset into the buffer text. Always on a UTF-8 char boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Offset(pub usize);

/// A logical (row, column) position. `column` counts UTF-16 code units on the
/// row (the IDE/JSON convention; `flux_text` caret mapping reconciles this
/// against glyph x). Row 0 is the first line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Point {
    pub row: u32,
    pub column: u32,
}

impl Point {
    pub const fn new(row: u32, column: u32) -> Self {
        Self { row, column }
    }
    pub const ZERO: Point = Point { row: 0, column: 0 };
}

/// A gap buffer: a single `Vec<u8>` with a movable gap. The gap sits between
/// `gap_start..gap_end`; logical text is `bytes[..gap_start]` followed by
/// `bytes[gap_end..]`.
pub struct GapBuffer {
    bytes: Vec<u8>,
    gap_start: usize,
    gap_end: usize,
    /// Cached byte offset of the start of each row (`line_starts[0] == 0`).
    /// Invalidated fully on any edit and rebuilt lazily by [`Self::ensure_lines`].
    line_starts: Vec<u32>,
    dirty: bool,
}

impl Default for GapBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl GapBuffer {
    /// New empty buffer with a 1 KiB gap.
    pub fn new() -> Self {
        let cap = 1024;
        Self {
            bytes: vec![0; cap],
            gap_start: 0,
            gap_end: cap,
            line_starts: vec![0],
            dirty: true,
        }
    }

    pub fn from_text(s: &str) -> Self {
        let mut b = Self::new();
        b.replace_all(s);
        b
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Logical length in bytes (excluding the gap).
    pub fn len(&self) -> usize {
        self.bytes.len() - (self.gap_end - self.gap_start)
    }

    /// Full text as an owned `String`. O(n).
    pub fn text(&self) -> String {
        let mut out = String::with_capacity(self.len());
        if let Ok(left) = std::str::from_utf8(&self.bytes[..self.gap_start]) {
            out.push_str(left);
        }
        if let Ok(right) = std::str::from_utf8(&self.bytes[self.gap_end..]) {
            out.push_str(right);
        }
        out
    }

    /// Substring `[lo, hi)` as an owned `String`. Clamps to bounds.
    pub fn slice(&self, range: Range<usize>) -> String {
        let len = self.len();
        let lo = range.start.min(len);
        let hi = range.end.min(len).max(lo);
        if lo == hi {
            return String::new();
        }
        let mut out = String::with_capacity(hi - lo);
        // Left part (bytes before the gap).
        if lo < self.gap_start {
            let r = lo..hi.min(self.gap_start);
            if let Ok(s) = std::str::from_utf8(&self.bytes[r]) {
                out.push_str(s);
            }
        }
        // Right part (bytes after the gap).
        if hi > self.gap_start {
            // Rebase logical offsets [max(lo,gap_start) .. hi) into physical
            // offsets by adding (gap_end - gap_start).
            let phys_lo = self.gap_end + (lo.max(self.gap_start) - self.gap_start);
            let phys_hi = self.gap_end + (hi - self.gap_start);
            if let Ok(s) = std::str::from_utf8(&self.bytes[phys_lo..phys_hi]) {
                out.push_str(s);
            }
        }
        out
    }

    /// Replace the entire contents.
    pub fn replace_all(&mut self, s: &str) {
        self.bytes.clear();
        self.bytes.extend_from_slice(s.as_bytes());
        self.bytes.resize(self.bytes.len() + 1024, 0); // fresh trailing gap
        self.gap_start = s.len();
        self.gap_end = self.bytes.len();
        self.dirty = true;
    }

    /// Move the gap so it starts at logical offset `pos`.
    fn move_gap(&mut self, pos: usize) {
        debug_assert!(pos <= self.len());
        if pos == self.gap_start {
            return;
        }
        if self.gap_end - self.gap_start < 8 {
            self.grow_gap(1024);
        }
        if pos < self.gap_start {
            // Move the block [pos, gap_start) to just past the gap end.
            let n = self.gap_start - pos;
            self.bytes
                .copy_within(pos..self.gap_start, self.gap_end - n);
            self.gap_end -= n;
            self.gap_start = pos;
        } else {
            // Move the block [gap_end, gap_end + delta) to just past gap_start.
            let n = pos - self.gap_start;
            self.bytes
                .copy_within(self.gap_end..self.gap_end + n, self.gap_start);
            self.gap_start += n;
            self.gap_end += n;
        }
    }

    fn grow_gap(&mut self, extra: usize) {
        let cur = self.gap_end - self.gap_start;
        if cur >= extra {
            return;
        }
        let need = (extra - cur) + cur / 2 + 1024;
        let mut new_bytes = Vec::with_capacity(self.bytes.len() + need);
        new_bytes.extend_from_slice(&self.bytes[..self.gap_start]);
        new_bytes.resize(new_bytes.len() + (self.gap_end - self.gap_start) + need, 0);
        new_bytes.extend_from_slice(&self.bytes[self.gap_end..]);
        self.gap_end += need;
        self.bytes = new_bytes;
    }

    /// Insert `text` at logical offset `pos` (a char boundary). O(move + len).
    pub fn insert(&mut self, pos: usize, text: &str) {
        if text.is_empty() {
            return;
        }
        let need = text.len();
        if self.gap_end - self.gap_start < need {
            self.grow_gap(need);
        }
        self.move_gap(pos);
        self.bytes[self.gap_start..self.gap_start + need].copy_from_slice(text.as_bytes());
        self.gap_start += need;
        self.dirty = true;
    }

    /// Delete `[lo, hi)` (clamped). O(move).
    pub fn delete(&mut self, range: Range<usize>) {
        let len = self.len();
        let lo = range.start.min(len);
        let hi = range.end.min(len).max(lo);
        if lo == hi {
            return;
        }
        self.move_gap(lo);
        self.gap_end += hi - lo;
        self.dirty = true;
    }

    /// Number of logical rows. Text with no trailing newline has `row_count`
    /// equal to the number of lines; the caret can always sit one row past the
    /// last content line (an empty trailing row).
    pub fn row_count(&self) -> u32 {
        let text = self.text();
        let lines = text.split('\n').count() as u32;
        lines.max(1)
    }

    fn ensure_lines(&mut self) -> &Vec<u32> {
        if !self.dirty {
            return &self.line_starts;
        }
        self.line_starts.clear();
        self.line_starts.push(0);
        let text = self.text();
        for (i, b) in text.bytes().enumerate() {
            if b == b'\n' {
                self.line_starts.push((i + 1) as u32);
            }
        }
        self.dirty = false;
        &self.line_starts
    }

    /// `Point` → `Offset`.
    pub fn offset_of_point(&mut self, p: Point) -> Offset {
        let line_starts = self.ensure_lines().clone();
        let row = (p.row as usize).min(line_starts.len().saturating_sub(1));
        let line_start = line_starts[row] as usize;
        let line_end = line_starts
            .get(row + 1)
            .map(|&s| (s as usize).saturating_sub(1)) // exclude the '\n'
            .unwrap_or_else(|| self.len());
        let line_text = self.slice(line_start..line_end);
        let mut units = 0u32;
        let mut byte_off = line_start;
        for c in line_text.chars() {
            if units >= p.column {
                break;
            }
            units += c.len_utf16() as u32;
            byte_off += c.len_utf8();
        }
        Offset(byte_off.min(self.len()))
    }

    /// `Offset` → `Point`.
    pub fn point_of_offset(&mut self, o: Offset) -> Point {
        let line_starts = self.ensure_lines().clone();
        let off = o.0.min(self.len());
        let row = match line_starts.binary_search(&(off as u32)) {
            Ok(i) => i,
            Err(i) => i.saturating_sub(1),
        };
        let line_start = line_starts[row] as usize;
        let prefix = self.slice(line_start..off);
        let column = prefix.chars().map(|c| c.len_utf16() as u32).sum();
        Point {
            row: row as u32,
            column,
        }
    }

    /// Next grapheme boundary after `o` (or `o` at EOF).
    pub fn next_grapheme(&self, o: Offset) -> Offset {
        let s = self.text();
        let mut cur = GraphemeCursor::new(o.0.min(s.len()), s.len(), true);
        match cur.next_boundary(&s, 0) {
            Ok(Some(b)) => Offset(b),
            _ => Offset(s.len()),
        }
    }

    /// Previous grapheme boundary before `o` (or `o` at BOF).
    pub fn prev_grapheme(&self, o: Offset) -> Offset {
        let s = self.text();
        let mut cur = GraphemeCursor::new(o.0.min(s.len()), s.len(), true);
        match cur.prev_boundary(&s, 0) {
            Ok(Some(b)) => Offset(b),
            _ => Offset(0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_delete_roundtrip() {
        let mut b = GapBuffer::from_text("hello");
        b.insert(5, " world");
        assert_eq!(b.text(), "hello world");
        b.delete(5..11);
        assert_eq!(b.text(), "hello");
    }

    #[test]
    fn slice_across_gap() {
        let mut b = GapBuffer::from_text("hello world");
        b.move_gap_pub(5); // gap now splits "hello" | " world"
        assert_eq!(b.slice(0..11), "hello world");
        assert_eq!(b.slice(3..8), "lo wo");
        assert_eq!(b.slice(6..11), "world");
    }

    #[test]
    fn point_offset_roundtrip() {
        let mut b = GapBuffer::from_text("abc\ndef\nghi");
        let p = b.point_of_offset(Offset(5)); // 'e' on row 1
        assert_eq!(p, Point::new(1, 1));
        let o = b.offset_of_point(Point::new(2, 2)); // 'i' on row 2
        assert_eq!(o, Offset(10));
    }

    #[test]
    fn grapheme_walk_handles_emoji() {
        let b = GapBuffer::from_text("a👍b");
        // 'a' then '👍' then 'b' → next from 0 lands at the emoji cluster start.
        assert_eq!(b.next_grapheme(Offset(0)), Offset(1));
    }

    #[test]
    fn row_count() {
        assert_eq!(GapBuffer::from_text("").row_count(), 1);
        assert_eq!(GapBuffer::from_text("one").row_count(), 1);
        assert_eq!(GapBuffer::from_text("a\nb").row_count(), 2);
        assert_eq!(GapBuffer::from_text("a\nb\n").row_count(), 3);
    }

    // Test-only access to move_gap so the cross-gap slice path is exercised.
    impl GapBuffer {
        fn move_gap_pub(&mut self, pos: usize) {
            self.move_gap(pos);
        }
    }
}
