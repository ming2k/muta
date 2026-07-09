//! Selections, modelled on Zed's `text::Selection<T>`.
//!
//! A [`Selection`] has a head (where the caret is) and an anchor (the other
//! end). A degenerate selection (head == anchor) is a bare caret. The editor
//! keeps a [`Selections`] collection so multi-cursor and columnar selection
//! work the same as single-cursor: every command maps over the collection.

use crate::buffer::Offset;

/// The direction a selection was made; influences how `expand` grows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SelectionGoal {
    #[default]
    None,
    Left,
    Right,
}

/// A selection generic over position type. Like Zed, generic so the same type
/// describes offset-selections and point-selections.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection<T> {
    pub head: T,
    pub anchor: T,
    pub goal: SelectionGoal,
}

impl<T: Copy + Ord> Selection<T> {
    pub fn new_caret(at: T) -> Self {
        Self {
            head: at,
            anchor: at,
            goal: SelectionGoal::None,
        }
    }

    pub fn new_range(start: T, end: T) -> Self {
        Self {
            head: end,
            anchor: start,
            goal: SelectionGoal::None,
        }
    }

    pub fn is_caret(&self) -> bool {
        self.head == self.anchor
    }

    /// Inclusive start (min of head/anchor).
    pub fn start(&self) -> T {
        self.head.min(self.anchor)
    }

    /// Exclusive end (max of head/anchor).
    pub fn end(&self) -> T {
        self.head.max(self.anchor)
    }

    /// Half-open containment: `[start, end)`.
    pub fn contains(&self, p: T) -> bool {
        p >= self.start() && p < self.end()
    }
}

impl<T: Copy + Ord + Default> Default for Selection<T> {
    fn default() -> Self {
        Self::new_caret(T::default())
    }
}

/// The editor's selection state. Always non-empty (`all[0]` is primary).
#[derive(Debug, Clone, Default)]
pub struct Selections {
    /// Sorted, non-overlapping offset selections. `all[0]` is primary.
    pub all: Vec<Selection<Offset>>,
}

impl Selections {
    pub fn new(at: Offset) -> Self {
        Self {
            all: vec![Selection::new_caret(at)],
        }
    }

    pub fn primary(&self) -> Selection<Offset> {
        self.all[0]
    }

    pub fn primary_mut(&mut self) -> &mut Selection<Offset> {
        &mut self.all[0]
    }

    /// Replace all selections with one caret at `at`.
    pub fn collapse_to(&mut self, at: Offset) {
        self.all.clear();
        self.all.push(Selection::new_caret(at));
    }

    /// Add a caret at `at`. Dedupes against an existing selection at the same
    /// head.
    pub fn add_caret(&mut self, at: Offset) {
        if self.all.iter().any(|s| s.head == at) {
            return;
        }
        self.all.push(Selection::new_caret(at));
        self.normalize();
    }

    /// Sort selections by start and merge overlapping ranges. Carets
    /// (zero-width) never merge with each other; only ranges that overlap or
    /// touch collapse — matching Zed's behaviour.
    fn normalize(&mut self) {
        self.all.sort_by_key(|s| s.start());
        let mut out: Vec<Selection<Offset>> = Vec::with_capacity(self.all.len());
        for s in self.all.drain(..) {
            match out.last_mut() {
                // Merge only if BOTH have width and they overlap/touch.
                Some(last) if !last.is_caret() && !s.is_caret() && s.start() <= last.end() => {
                    let new_start = last.start();
                    let new_end = last.end().max(s.end());
                    last.anchor = new_start;
                    last.head = new_end;
                }
                _ => out.push(s),
            }
        }
        self.all = out;
        if self.all.is_empty() {
            self.all.push(Selection::default());
        }
    }

    /// Rebase every selection's offsets through a single contiguous edit:
    /// bytes `[deleted.start, deleted.end)` were removed and `inserted_len`
    /// bytes inserted at `deleted.start`. Carets at/before the insert point
    /// stay; carets inside the deleted range snap to the insert point; carets
    /// after shift by the net delta. This is the sequential-editing cousin of
    /// Zed's anchors — correct for one user, no collaboration.
    pub fn apply_edit(&mut self, deleted: std::ops::Range<Offset>, inserted_len: usize) {
        let d_lo = deleted.start.0;
        let d_hi = deleted.end.0;
        for s in &mut self.all {
            for pos in [&mut s.head, &mut s.anchor] {
                let p = pos.0;
                if p <= d_lo {
                    // before the edit — unchanged
                } else if p >= d_hi {
                    // after the edit — shift by net delta
                    pos.0 = d_lo + inserted_len + (p - d_hi);
                } else {
                    // inside the deleted range — clamp to insert point
                    pos.0 = d_lo;
                }
            }
        }
        self.normalize();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caret_vs_range() {
        let c = Selection::new_caret(Offset(3));
        assert!(c.is_caret());
        assert_eq!(c.start(), Offset(3));
        assert_eq!(c.end(), Offset(3));

        let r = Selection::new_range(Offset(3), Offset(7));
        assert!(!r.is_caret());
        assert_eq!(r.start(), Offset(3));
        assert_eq!(r.end(), Offset(7));
        assert!(r.contains(Offset(5)));
        assert!(!r.contains(Offset(7)));
    }

    #[test]
    fn ranges_merge_but_carets_do_not() {
        let mut s = Selections::new(Offset(0));
        // Two overlapping ranges [3,7) and [5,9) → merge to [3,9).
        s.all.push(Selection::new_range(Offset(3), Offset(7)));
        s.all.push(Selection::new_range(Offset(5), Offset(9)));
        s.normalize();
        assert_eq!(s.all.len(), 2); // [0 caret] + merged range
        assert_eq!(s.all[1].start(), Offset(3));
        assert_eq!(s.all[1].end(), Offset(9));
    }

    #[test]
    fn apply_edit_shifts_after() {
        let mut s = Selections::new(Offset(10));
        // Insert 5 bytes at offset 2: everything >= 2 shifts by +5.
        s.apply_edit(Offset(2)..Offset(2), 5);
        assert_eq!(s.primary().head, Offset(15));
    }

    #[test]
    fn apply_edit_inside_delete_clamps() {
        let mut s = Selections::new(Offset(5));
        // Delete [2,8): caret at 5 is inside → clamps to 2.
        s.apply_edit(Offset(2)..Offset(8), 0);
        assert_eq!(s.primary().head, Offset(2));
    }
}
