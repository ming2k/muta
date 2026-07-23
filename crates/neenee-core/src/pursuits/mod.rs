//! Pursuit domain types (ADR-0005 pure-domain half).
//!
//! The persisted/I/O-bound layer — the `rusqlite`-backed `PursuitStore` and the
//! `PursuitService` facade — lives in `neenee_persistence::pursuits`. This module
//! keeps only the domain shapes a frontend needs without pulling in SQLite:
//! `Pursuit` (runtime view), `ThreadPursuit` (persisted view), `TokenUsage`,
//! `RoundOutcome`, and the per-turn `RoundTimer`. The pursuit lifecycle is driven
//! by the `/pursue` slash command, the in-turn stop-gate, and the
//! `[NEENEE_PURSUIT_COMPLETE]` marker; there are no model-facing pursuit tools
//! (ADR-0031).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::time::Instant;

/// The persisted view of a thread/session pursuit.
///
/// Slimmed in ADR-0010: the status machine, token budget, and elapsed-time
/// accounting are gone. Only `objective`, `is_complete`, and timestamps
/// persist. The `thread_pursuits` table still carries the legacy
/// `token_budget` / `tokens_used` / `time_used_seconds` columns for
/// backward compatibility with pre-0010 databases, but they are no longer
/// read or written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadPursuit {
    pub thread_id: String,
    pub pursuit_id: String,
    pub objective: String,
    pub is_complete: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

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

/// Token usage reported by a single turn.
///
/// Per-turn telemetry only — not booked against any pursuit (ADR-0010 removed
/// pursuit-level token accounting).
///
/// `cache_creation_input_tokens` / `cache_read_input_tokens` carry prompt-cache
/// counts. Anthropic reports both: its `input_tokens` is ONLY the uncached
/// dynamic suffix, so the cache write/read counts must be tracked separately
/// (and added into `prompt_tokens`/`total_tokens`) or the context meter would
/// undercount every cached turn. OpenAI / Gemini / Moonshot auto-cache (or
/// session-key cache) and surface the hit as a single read count — their
/// `cache_creation_input_tokens` stays zero. The shared parser lives in
/// [`crate::cache`](crate::cache); see [`crate::CachePolicy`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub total_tokens: i64,
    /// Tokens written to the prompt cache this turn (billed at a premium by
    /// Anthropic; absent on providers without explicit breakpoint caching).
    pub cache_creation_input_tokens: i64,
    /// Tokens served from the prompt cache this turn (billed at a steep
    /// discount by Anthropic; surfaced as `cached_tokens` /
    /// `cachedContentTokenCount` by the auto-caching providers).
    pub cache_read_input_tokens: i64,
}

/// Outcome returned by the agent after running one turn.
#[derive(Debug, Clone)]
pub struct RoundOutcome {
    pub message: crate::Message,
    pub token_usage: TokenUsage,
    pub duration_ms: u64,
}

/// Turn-level elapsed-time keeper. Kept after ADR-0010 even though
/// pursuit-level time accounting is gone, because the harness still uses it
/// for per-turn telemetry (e.g. plan-progress timestamps).
pub struct RoundTimer {
    start: Instant,
}

impl Default for RoundTimer {
    fn default() -> Self {
        Self::new()
    }
}

impl RoundTimer {
    pub fn new() -> Self {
        Self {
            start: Instant::now(),
        }
    }

    pub fn elapsed_seconds(&self) -> i64 {
        self.start.elapsed().as_secs() as i64
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
