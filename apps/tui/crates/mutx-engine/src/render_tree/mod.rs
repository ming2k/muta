//! Retained Render Tree and Layout Engine (ADR-0195).
//!
//! Provides a first-principles, retain-mode UI tree architecture:
//! - `BoxConstraints`: Downward layout constraints (min/max width and height).
//! - `Size`: Upward geometry decision (actual width and height).
//! - `Offset`: Spatial coordinate translation.
//! - `RenderBox`: Fundamental trait for nodes that measure, layout, paint, and hit-test.
//! - `PaintContext`: Scoped drawing context with offset stacking and clip isolation.
//! - `HitTestResult`: Event routing target collection.
//! - `RenderTree`: Root coordinator for layout, paint, and hit-testing passes.

pub mod boxes;
pub mod constraints;
pub mod context;
pub mod render_box;
pub mod tree;

pub use boxes::block::RenderBlock;
pub use boxes::clip::RenderClip;
pub use boxes::flex::{CrossAxisAlignment, FlexChild, FlexDirection, FlexFit, MainAxisAlignment, RenderFlex};
pub use boxes::leaf::{RenderCustom, RenderLeaf};
pub use boxes::padding::RenderPadding;
pub use boxes::paragraph::RenderParagraph;
pub use boxes::stack::{Alignment, RenderStack, StackChild, StackFit};
pub use constraints::{BoxConstraints, Offset, Size};
pub use context::{HitTestEntry, HitTestResult, PaintContext};
pub use render_box::RenderBox;
pub use tree::RenderTree;
