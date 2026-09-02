//! Transient chrome around the input box: the activity bar with an animated
//! breathing-dot indicator, the one-row todo bar that surfaces the live task
//! list, the completion menu anchored above the input, and the persistent
//! model bar pinned below the input (context usage, stream rate, model
//! identity).

pub mod activity_bar;
pub mod common;
pub mod completion_menu;
pub mod model_bar;
pub mod queue_bar;
pub mod step_focus_bar;
pub mod todo_bar;

#[cfg(test)]
mod tests;

#[allow(unused_imports)]
pub use activity_bar::{ActivityBarView, draw_activity_bar};
#[allow(unused_imports)]
pub use common::{
    Liveness, SPINNER_PHASES, breathing_color, classify_liveness, dot_color, format_elapsed,
    spinner_glyph, tilde_home,
};
pub use completion_menu::draw_completion_menu;
#[allow(unused_imports)]
pub use model_bar::{
    CONTEXT_USAGE_CRIT_THRESHOLD, CONTEXT_USAGE_WARN_THRESHOLD, ModelBarRects, ModelBarView,
    draw_model_bar, format_token_count,
};
pub use queue_bar::{QueueBarView, QueueItemView, draw_queue_bar};
pub use step_focus_bar::draw_step_focus_bar;
pub use todo_bar::draw_todo_bar;
