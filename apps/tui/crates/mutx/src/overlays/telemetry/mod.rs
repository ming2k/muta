//! Session Stats modal: unified context usage and performance stats
//! grouped by user round, with turn-level drill-down and attempt inspection.

pub mod draw;
pub mod model;

#[cfg(test)]
mod tests;

pub use draw::draw_telemetry_modal;
pub use model::{
    ContextUsageProps, telemetry_attempt_count, telemetry_attempt_key, telemetry_round_count,
};

#[derive(PartialEq, Eq, Hash, Clone, Copy, Debug, Default)]
pub enum TelemetryTab {
    #[default]
    Overview,
    Activity,
}

impl TelemetryTab {
    pub fn label(self) -> &'static str {
        match self {
            TelemetryTab::Overview => "Overview",
            TelemetryTab::Activity => "Activity",
        }
    }
}
