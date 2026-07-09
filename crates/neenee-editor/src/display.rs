//! Buffer → display mapping (`DisplayMap`-lite).
//!
//! Hard-wrap each logical row to `max_width` logical px (via optics's
//! `flux_text_layout`) and split on `\n`. Produces [`DisplayLine`]s the
//! renderer iterates. No folds/inlays yet — those are additive later.
//!
//! Each [`DisplayLine`] carries the *absolute* buffer byte range of its visible
//! text plus the source logical row (for the gutter). Caret/selection mapping
//! in `render.rs` then works directly in absolute byte offsets against
//! `flux_text`.

use flux_text::{Style, Text};
use flux_text_layout::wrap;

/// One visual line: an absolute byte range into the buffer + its source row.
#[derive(Debug, Clone, Copy)]
pub struct DisplayLine {
    pub lo: usize,
    pub hi: usize,
    pub row: u32,
}

/// The display mapping for a frame: visual lines, plus each logical row's
/// absolute start byte (so the renderer can rebaseline caret offsets).
pub struct DisplayMap {
    pub lines: Vec<DisplayLine>,
    /// Absolute byte offset of the start of each logical row.
    pub row_starts: Vec<usize>,
}

/// Build the display map by wrapping each logical row.
///
/// `rows` yields `(row_index, row_text)`. `max_width` is in logical px and must
/// match the width the renderer wraps to, so caret x is consistent.
pub fn build(
    text: &Text,
    style: &Style,
    rows: impl IntoIterator<Item = (u32, String)>,
    max_width: f32,
) -> DisplayMap {
    let mut lines = Vec::new();
    let mut row_starts = Vec::new();
    let mut abs = 0usize;
    for (row, row_text) in rows {
        row_starts.push(abs);
        if row_text.is_empty() {
            lines.push(DisplayLine {
                lo: abs,
                hi: abs,
                row,
            });
            // The row_text is empty; abs is unchanged (the trailing '\n', if
            // any, is accounted for by the caller splitting on '\n').
            continue;
        }
        let wrapped = wrap(text, &row_text, style, max_width);
        for wl in wrapped {
            lines.push(DisplayLine {
                lo: abs + wl.lo,
                hi: abs + wl.hi,
                row,
            });
        }
        abs += row_text.len();
    }
    if row_starts.is_empty() {
        row_starts.push(0);
        lines.push(DisplayLine {
            lo: 0,
            hi: 0,
            row: 0,
        });
    }
    DisplayMap { lines, row_starts }
}
