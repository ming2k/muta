//! Pursuit domain values: `Pursuit` and `PursuitBudget`.
//!
//! Pursuit persistence lives on `SessionStore` (`SessionData.pursuit`,
//! ADR-0032), not here. The lifecycle is driven by the `/pursue` slash
//! command, the in-turn stop-gate, and the `[NEENEE_PURSUIT_COMPLETE]`
//! marker; there are no model-facing pursuit tools (ADR-0031). Budgets and
//! the terminal reason are ADR-0069.

use serde::{Deserialize, Serialize};

/// The runtime view of a pursuit exposed to the agent and TUI.
///
/// Carries the durable `objective`, a single `is_complete` flag that mirrors
/// the persisted column, and an optional [`PursuitBudget`] the user may set to
/// bound the autonomous loop (ADR-0069). Runtime counters (turns/tokens/wall
/// clock) live on [`PursuitState`] in the agent crate, not here: they are
/// session-scoped and rebuilt on resume, so they are not part of the durable
/// pursuit record.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pursuit {
    pub objective: String,
    #[serde(default)]
    pub is_complete: bool,
    /// Optional hard budgets (turn / token / wall-clock). `None` means uncapped
    /// (the default). Set via `/pursue budget …`; reaching any budget stops the
    /// loop and marks the pursuit blocked with a terminal reason.
    #[serde(default)]
    pub budget: Option<PursuitBudget>,
    /// Why the loop stopped, when it stopped for a non-completion reason
    /// (budget reached, interrupted, blocked). `None` while active or on
    /// successful completion. Mirrors kimi-code's `terminalReason`.
    #[serde(default)]
    pub terminal_reason: Option<String>,
}

impl Pursuit {
    /// A fresh, active pursuit for `objective` with no budget and no terminal
    /// reason.
    pub fn new(objective: impl Into<String>) -> Self {
        Self {
            objective: objective.into(),
            ..Default::default()
        }
    }
}

/// Hard budget for a pursuit loop (ADR-0069). Every field is optional; a budget
/// with all fields `None` is equivalent to no budget. Only fields the user set
/// explicitly are enforced. Borrowed from kimi-code's `GoalBudgetLimits`, but
/// opt-in only — fuzzy expressions like "soon" never set a budget.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PursuitBudget {
    /// Maximum number of continuation turns. `None` = uncapped on turns.
    #[serde(default)]
    pub max_turns: Option<u32>,
    /// Maximum cumulative tokens (prompt + completion) across all pursuit turns.
    /// `None` = uncapped on tokens.
    #[serde(default)]
    pub max_tokens: Option<u64>,
    /// Maximum wall-clock duration in milliseconds the loop may run. `None` =
    /// uncapped on time.
    #[serde(default)]
    pub max_wall_clock_ms: Option<u64>,
}

impl PursuitBudget {
    /// Whether every field is `None` (i.e. the budget imposes no constraint).
    pub const fn is_empty(self) -> bool {
        self.max_turns.is_none() && self.max_tokens.is_none() && self.max_wall_clock_ms.is_none()
    }

    /// The highest usage fraction across the set budgets (0.0–1.0+), or `None`
    /// when no budget is set. Used to switch the continuation prompt from
    /// "steady progress" to "converge" past 75%.
    pub fn usage_fraction(self, turns: u32, tokens: u64, elapsed_ms: u64) -> Option<f64> {
        let mut max: Option<f64> = None;
        if let Some(cap) = self.max_turns
            && cap > 0
        {
            max = Some(turns as f64 / cap as f64);
        }
        if let Some(cap) = self.max_tokens
            && cap > 0
        {
            let frac = tokens as f64 / cap as f64;
            max = Some(max.map_or(frac, |m| m.max(frac)));
        }
        if let Some(cap) = self.max_wall_clock_ms
            && cap > 0
        {
            let frac = elapsed_ms as f64 / cap as f64;
            max = Some(max.map_or(frac, |m| m.max(frac)));
        }
        max
    }

    /// Whether the given usage has reached or exceeded any set budget.
    pub fn is_exceeded(self, turns: u32, tokens: u64, elapsed_ms: u64) -> bool {
        self.max_turns.is_some_and(|cap| turns >= cap)
            || self.max_tokens.is_some_and(|cap| tokens >= cap)
            || self.max_wall_clock_ms.is_some_and(|cap| elapsed_ms >= cap)
    }

    /// A short human-readable reason when a budget is exceeded, identifying
    /// which axis tripped. `None` when no budget is exceeded.
    pub fn exceeded_reason(self, turns: u32, tokens: u64, elapsed_ms: u64) -> Option<String> {
        if let Some(cap) = self.max_turns
            && turns >= cap
        {
            return Some(format!("turn budget reached ({turns}/{cap})"));
        }
        if let Some(cap) = self.max_tokens
            && tokens >= cap
        {
            return Some(format!("token budget reached ({tokens}/{cap})"));
        }
        if let Some(cap) = self.max_wall_clock_ms
            && elapsed_ms >= cap
        {
            return Some(format!("time budget reached ({elapsed_ms}ms/{cap}ms)"));
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pursuit_new_is_active_uncapped() {
        let p = Pursuit::new("ship it");
        assert_eq!(p.objective, "ship it");
        assert!(!p.is_complete);
        assert!(p.budget.is_none());
        assert!(p.terminal_reason.is_none());
    }

    #[test]
    fn empty_budget_imposes_no_constraint() {
        assert!(PursuitBudget::default().is_empty());
        assert!(
            !PursuitBudget {
                max_turns: Some(10),
                ..Default::default()
            }
            .is_empty()
        );
    }

    #[test]
    fn usage_fraction_tracks_the_tightest_axis() {
        let b = PursuitBudget {
            max_turns: Some(20),
            max_tokens: Some(1000),
            ..Default::default()
        };
        // 10/20 turns = 0.5; 200/1000 tokens = 0.2 → max 0.5.
        assert!((b.usage_fraction(10, 200, 0).unwrap() - 0.5).abs() < 1e-9);
        // 18/20 turns = 0.9; 900/1000 tokens = 0.9 → 0.9.
        assert!((b.usage_fraction(18, 900, 0).unwrap() - 0.9).abs() < 1e-9);
    }

    #[test]
    fn usage_fraction_none_when_no_budget_set() {
        assert!(
            PursuitBudget::default()
                .usage_fraction(99, 9999, 9999)
                .is_none()
        );
    }

    #[test]
    fn budget_exceeded_detects_each_axis() {
        let b = PursuitBudget {
            max_turns: Some(5),
            max_tokens: Some(1000),
            max_wall_clock_ms: Some(60_000),
        };
        assert!(b.is_exceeded(5, 0, 0)); // turns
        assert!(b.is_exceeded(0, 1000, 0)); // tokens
        assert!(b.is_exceeded(0, 0, 60_000)); // time
        assert!(!b.is_exceeded(4, 999, 59_999)); // under all
    }

    #[test]
    fn exceeded_reason_names_the_axis() {
        let b = PursuitBudget {
            max_turns: Some(5),
            max_tokens: Some(1000),
            max_wall_clock_ms: Some(60_000),
        };
        assert_eq!(
            b.exceeded_reason(5, 0, 0).as_deref(),
            Some("turn budget reached (5/5)")
        );
        // turns not exceeded, tokens exceeded → token reason.
        assert_eq!(
            b.exceeded_reason(2, 1000, 0).as_deref(),
            Some("token budget reached (1000/1000)")
        );
        // nothing exceeded → None.
        assert!(b.exceeded_reason(1, 1, 1).is_none());
    }

    #[test]
    fn budget_serde_round_trips_with_defaults() {
        // A legacy pursuit JSON (pre-budget) must still deserialize: missing
        // `budget` / `terminal_reason` default to None.
        let json = r#"{"objective":"x","is_complete":false}"#;
        let p: Pursuit = serde_json::from_str(json).unwrap();
        assert_eq!(p.objective, "x");
        assert!(p.budget.is_none());
        assert!(p.terminal_reason.is_none());
    }
}
