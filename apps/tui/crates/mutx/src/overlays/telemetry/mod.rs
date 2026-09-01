//! Session Telemetry modal: unified context usage and performance telemetry
//! grouped by user round, with turn-level drill-down and attempt inspection.

pub mod model;
pub mod view;

#[cfg(test)]
mod tests;

pub use model::{
    ContextUsageView, telemetry_attempt_count, telemetry_attempt_key, telemetry_round_count,
};
pub use view::draw_telemetry_modal;
