//! Step rendering implementation: the summary primitives, the per-tool body
//! content renderers, and the top-level orchestrators.

pub mod base;
pub mod command;
pub mod payloads;
pub mod reasoning;
pub mod runner;
pub mod sticky;
pub mod tools;

#[cfg(test)]
mod tests;

pub(crate) use base::RenderCtx;
pub use command::draw_command_result;
pub use reasoning::draw_reasoning_trace;
pub use runner::draw_runner_inline_step;
pub use sticky::{StickyStep, draw_sticky_summary_if_needed};
pub use tools::draw_tool_step;
