//! Undo/redo via an edit-log. Coalesces consecutive same-kind edits (typing
//! runs, backspace runs) into one transaction, mirroring Zed's `Transaction`.
//!
//! Each [`Edit`] is `delete` (a range removed) then `insert` (text added at
//! `delete.start`). A [`Transaction`] groups edits undone/redone together;
//! `coalesce_key` merges a transaction into its predecessor when both share it,
//! so a run of typed characters collapses to a single undo step.

use std::ops::Range;

use crate::buffer::{GapBuffer, Offset};

/// One atomic edit: delete `[delete.start, delete.end)` then insert `insert`
/// at `delete.start`. `deleted_text` is the text that *was* in `[delete.start,
/// delete.end)` before the edit — captured at apply time so undo can restore it
/// without re-deriving it from a buffer that has already moved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    pub delete: Range<Offset>,
    pub insert: String,
    pub deleted_text: String,
}

/// A group of edits undone/redone together.
#[derive(Debug, Clone)]
pub struct Transaction {
    pub edits: Vec<Edit>,
    pub coalesce_key: Option<u8>,
}

/// Coalescing keys. Equal key on consecutive top-of-stack transactions ⇒ merge.
pub const KEY_TYPE: u8 = 1;
pub const KEY_BACKSPACE: u8 = 2;
pub const KEY_DELETE: u8 = 3;
pub const KEY_PASTE: u8 = 4;

#[derive(Debug, Default)]
pub struct History {
    undo: Vec<Transaction>,
    redo: Vec<Transaction>,
    /// Edits collected for the transaction currently being typed.
    pub in_progress: Vec<Edit>,
    coalesce_key: Option<u8>,
}

impl History {
    /// Begin a transaction that should coalesce with the previous one iff it
    /// shares `key`. If the previous committed transaction has the same key
    /// (and nothing has been undone since), we pull its edits back into
    /// `in_progress` so the new edits append to it.
    pub fn begin(&mut self, key: u8) {
        self.coalesce_key = Some(key);
        if self.in_progress.is_empty()
            && self
                .undo
                .last()
                .is_some_and(|t| t.coalesce_key == Some(key))
            && let Some(mut t) = self.undo.pop()
        {
            self.in_progress.append(&mut t.edits);
        }
    }

    pub fn record(&mut self, edit: Edit) {
        self.in_progress.push(edit);
    }

    pub fn commit(&mut self) {
        if !self.in_progress.is_empty() {
            self.undo.push(Transaction {
                edits: std::mem::take(&mut self.in_progress),
                coalesce_key: self.coalesce_key,
            });
            self.redo.clear();
        }
        self.coalesce_key = None;
    }

    /// Undo the last transaction: apply each edit's *inverse* to `buf`, in
    /// reverse order. Pushes the original (forward) edits onto the redo stack
    /// so a subsequent `redo` replays them. Returns the byte range the first
    /// inverted edit touched (for selection restoration).
    pub fn undo(&mut self, buf: &mut GapBuffer) -> Option<Range<Offset>> {
        let t = self.undo.pop()?;
        let forward: Vec<Edit> = t.edits.clone();
        let mut affected = Offset(0)..Offset(0);
        for e in t.edits.iter().rev() {
            // Inverse: delete the inserted span, restore the deleted text.
            let inv_lo = e.delete.start.0;
            let inv_hi = e.delete.start.0 + e.insert.len();
            buf.delete(inv_lo..inv_hi);
            buf.insert(inv_lo, &e.deleted_text);
            affected = Offset(inv_lo)..Offset(inv_lo + e.deleted_text.len());
        }
        self.redo.push(Transaction {
            edits: forward,
            coalesce_key: None,
        });
        Some(affected)
    }

    /// Redo the last undone transaction: replay each edit forward.
    pub fn redo(&mut self, buf: &mut GapBuffer) -> Option<Range<Offset>> {
        let t = self.redo.pop()?;
        let mut affected = Offset(0)..Offset(0);
        for e in &t.edits {
            buf.delete(e.delete.start.0..e.delete.end.0);
            buf.insert(e.delete.start.0, &e.insert);
            affected = Offset(e.delete.start.0)..Offset(e.delete.start.0 + e.insert.len());
        }
        self.undo.push(t);
        Some(affected)
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty() || !self.in_progress.is_empty()
    }
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Clear all history (used after a wholesale `replace_all`).
    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
        self.in_progress.clear();
        self.coalesce_key = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn undo_redo_roundtrip() {
        let mut buf = GapBuffer::from_text("hello");
        let mut h = History::default();
        h.begin(KEY_TYPE);
        h.record(Edit {
            delete: Offset(5)..Offset(5),
            insert: " world".into(),
            deleted_text: String::new(),
        });
        h.commit();
        buf.insert(5, " world");
        assert_eq!(buf.text(), "hello world");

        h.undo(&mut buf);
        assert_eq!(buf.text(), "hello");

        h.redo(&mut buf);
        assert_eq!(buf.text(), "hello world");
    }

    #[test]
    fn coalesce_typing_into_one_undo() {
        let mut buf = GapBuffer::from_text("");
        let mut h = History::default();
        // type 'a','b','c' — each its own begin/commit but same key.
        for (i, ch) in ['a', 'b', 'c'].iter().enumerate() {
            h.begin(KEY_TYPE);
            h.record(Edit {
                delete: Offset(i)..Offset(i),
                insert: ch.to_string(),
                deleted_text: String::new(),
            });
            h.commit();
            buf.insert(i, &ch.to_string());
        }
        assert_eq!(buf.text(), "abc");
        // One undo should clear all three (coalesced).
        h.undo(&mut buf);
        assert_eq!(buf.text(), "");
    }

    #[test]
    fn different_keys_do_not_coalesce() {
        let mut buf = GapBuffer::from_text("");
        let mut h = History::default();
        h.begin(KEY_TYPE);
        h.record(Edit {
            delete: Offset(0)..Offset(0),
            insert: "a".into(),
            deleted_text: String::new(),
        });
        h.commit();
        buf.insert(0, "a");

        h.begin(KEY_BACKSPACE); // different key → new transaction
        h.record(Edit {
            delete: Offset(0)..Offset(1),
            insert: String::new(),
            deleted_text: "a".into(),
        });
        h.commit();
        buf.delete(0..1);

        // Two undos needed to get back to empty.
        h.undo(&mut buf);
        assert_eq!(buf.text(), "a");
        h.undo(&mut buf);
        assert_eq!(buf.text(), "");
    }
}
