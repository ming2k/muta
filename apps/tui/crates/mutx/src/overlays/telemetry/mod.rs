//! Session Telemetry modal: unified context usage and performance telemetry
//! grouped by user round, with turn-level drill-down and attempt inspection.

pub mod draw;
pub mod model;

#[cfg(test)]
mod tests;

pub use draw::draw_telemetry_modal;
pub use model::{
    ContextUsageProps, telemetry_attempt_count, telemetry_attempt_key, telemetry_round_count,
};
