//! neenee-editor — a Zed-influenced code editor rendered through optics.
//!
//! The architecture mirrors Zed's layering, kept small enough to read in one
//! sitting:
//!
//! - [`buffer`]    — line-aware UTF-8 gap buffer + [`buffer::Offset`] /
//!   [`buffer::Point`] positions + grapheme walks.
//! - [`selection`] — [`selection::Selection`] (head + anchor) and a
//!   multi-selection [`selection::Selections`] collection for multi-cursor.
//! - [`history`]   — transaction-based undo/redo with edit coalescing.
//! - [`display`]   — buffer → wrapped visual lines (`DisplayMap`-lite), built
//!   on optics's `flux_text_layout`.
//! - [`editor`]    — the controller: commands (movement, editing) → edits.
//! - [`render`]    — flux/flux-text document painting inside iris's paint
//!   callback (under the lens chrome). `gui` feature only.
//!
//! The `gui` feature wires the whole thing to an iris window in `main.rs`.
//! Without it the crate is a pure-Rust headless text engine that builds and
//! tests with no GPU or compositor — `buffer`/`selection`/`history`/`editor`
//! have no optics dependency at all.

pub mod buffer;
pub mod editor;
pub mod history;
pub mod selection;

#[cfg(feature = "gui")]
pub mod display;
#[cfg(feature = "gui")]
pub mod render;
