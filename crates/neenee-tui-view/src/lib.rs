//! neenee TUI **view layer** — paints the agent transcript and its overlays
//! into the [`neenee_tui_engine`] grid.
//!
//! This crate sits between the in-house [`neenee_tui_engine`] engine (a retained
//! cell grid with dirty tracking and a back/front diff; ADR-0038) and the app
//! shell (`neenee::tui`, which owns `App` state, the event loop, and input
//! mapping). It paints neenee_core domain types — so it depends on
//! [`neenee_core`] — but it never depends on the shell: the seam is the
//! borrowed [`transcript::TranscriptView`] struct the shell fills in each frame.
//!
//! Layering:
//! ```text
//! neenee-tui-engine (engine: cell grid, diff, crossterm backend)
//!         ▲ paint into the grid
//! neenee-tui-view (THIS crate: drawing + document model)   depends on neenee-core
//!         ▲ TranscriptView<'a> seam
//! neenee::tui (app shell: App, event loop, input)
//! ```
//!
//! Layout — the drawing tree sits at the crate root, grouped by concern:
//! - [`model`] — semantic data: the document, the rendered layout map
//!   (hit-testing), and selection state.
//! - [`transcript`] — the transcript-area renderer: [`transcript::draw_transcript`],
//!   [`transcript::TranscriptView`], height cache, and the re-exported drawing
//!   surface (chrome, composer, overlays, theme, …).
//! - [`components`] / [`overlays`] / [`tools`] / [`disclosure`] — the drawing
//!   sub-trees (reusable components, modal overlays, per-tool step renderers,
//!   expandable-step disclosure).
//! - [`layout`] — transcript arrangement strategies (`default` / `legacy`).
//! - [`theme`] / [`design`] / [`chrome`] / [`composer`] / [`primitives`] / … —
//!   drawing leaves and shared tokens.
//! - [`fuzzy`] / [`providers`] / [`modal`] / [`completion`] — misc helpers shared
//!   with the shell.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

// Semantic data model.
pub mod model;

// Drawing tree.
pub mod components;
pub mod disclosure;
pub mod layout;
pub mod overlays;
pub mod tools;

// Drawing leaves + shared tokens.
pub mod chrome;
pub mod composer;
pub mod design;
pub mod empty_state;
pub mod markdown_table;
pub mod message_body;
pub mod notice;
pub mod page_header;
pub mod primitives;
pub mod text_layout;
pub mod theme;
pub mod time;

// Transcript-area renderer (the entry point the app drives each frame).
pub mod view;
// Re-export the transcript renderer's surface at the crate root: the drawing
// leaves used to reach these via their old `paint` parent's namespace, so the
// crate root now stands in as that parent.
pub(crate) use view::*;

// Misc helpers shared with the shell.
pub mod completion;
pub mod fuzzy;
pub mod modal;
pub mod providers;

#[cfg(test)]
mod snapshot_tests;
