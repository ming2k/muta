//! In-memory pursuit state, extracted from the `Agent` god-object.
//!
//! Holds the three pieces of mutable pursuit state that used to be separate
//! `Arc<Mutex<…>>` fields on [`crate::Agent`]:
//!
//! - the active [`Pursuit`] (if any),
//! - whether the stop-gate is armed,
//! - the iteration counter driven by the stop-gate.
//!
//! The [`crate::Agent`] owns a single `PursuitState` and delegates its
//! pursuit-related public methods here, keeping the existing call sites
//! (`agent.get_pursuit()`, `agent.arm_pursuit()`, …) unchanged.

use std::sync::{Arc, Mutex};

use neenee_core::Pursuit;

use crate::pursuit_prompts;

/// Internal lock-guard helper: poison-immune (recovers via `into_inner`).
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Runtime counters accumulated against an armed pursuit (ADR-0083).
///
/// These are session-scoped: they start at zero when a fresh pursuit attempt
/// is armed and are mirrored into the separate durable pursuit-runtime record
/// at continuation save points. They exist so the [`PursuitBudget`] can be
/// enforced across crash recovery and a convergence reminder can fire past
/// 75%.
#[derive(Debug, Clone, Copy, Default)]
pub struct PursuitStats {
    /// Pursuit passes charged at stop-gate boundaries. This is one greater
    /// than `iterations` while a pass has completed: `iterations` counts only
    /// the continuations the gate actually forced.
    pub passes: u32,
    /// Cumulative tokens (prompt + completion) across all pursuit passes.
    pub tokens: u64,
    /// Wall-clock milliseconds spent in active pursuit passes.
    pub wall_clock_ms: u64,
}

impl PursuitStats {
    /// Book one pursuit pass's usage into the running totals.
    fn book_pass(&mut self, usage: neenee_core::TokenUsage, duration_ms: u64) {
        self.passes += 1;
        self.tokens = self.tokens.saturating_add(usage.total_tokens.max(0) as u64);
        self.wall_clock_ms = self.wall_clock_ms.saturating_add(duration_ms);
    }
}

/// In-memory runtime view of pursuit state.
///
/// Cheap to construct; all fields are `Arc<Mutex<_>>` so a clone is a shallow
/// share (used to hand the same pursuit state to the pursuit tools' context
/// without a lifetime tie to the agent).
#[derive(Clone)]
pub struct PursuitState {
    pursuit: Arc<Mutex<Option<Pursuit>>>,
    armed: Arc<Mutex<bool>>,
    iterations: Arc<Mutex<u32>>,
    /// Per-pursuit runtime counters (passes/tokens/wall-clock). Reset on arm.
    stats: Arc<Mutex<PursuitStats>>,
}

impl Default for PursuitState {
    fn default() -> Self {
        Self {
            pursuit: Arc::new(Mutex::new(None)),
            armed: Arc::new(Mutex::new(false)),
            iterations: Arc::new(Mutex::new(0)),
            stats: Arc::new(Mutex::new(PursuitStats::default())),
        }
    }
}

impl PursuitState {
    pub fn new() -> Self {
        Self::default()
    }

    // ── active pursuit ──────────────────────────────────────────────────

    pub fn get(&self) -> Option<Pursuit> {
        lock(&self.pursuit).clone()
    }

    pub fn set(&self, pursuit: Pursuit) {
        *lock(&self.pursuit) = Some(pursuit);
    }

    pub fn restore(&self, pursuit: Pursuit) {
        *lock(&self.pursuit) = Some(pursuit);
    }

    pub fn clear(&self) {
        *lock(&self.pursuit) = None;
    }

    pub fn can_complete(&self) -> bool {
        self.get().is_some()
    }

    // ── stop-gate ───────────────────────────────────────────────────────

    /// Arm the stop-gate and reset the iteration counter + runtime stats.
    ///
    /// A new execution attempt clears the previous attempt's terminal reason;
    /// the durable objective and its completion flag remain unchanged.
    pub fn arm(&self) {
        if let Some(mut pursuit) = self.get()
            && !pursuit.is_complete
        {
            pursuit.terminal_reason = None;
            self.set(pursuit);
        }
        *lock(&self.iterations) = 0;
        *lock(&self.stats) = PursuitStats::default();
        *lock(&self.armed) = true;
    }

    /// Resume an execution attempt restored from durable runtime state.
    /// Unlike [`Self::arm`], this preserves its iteration and budget counters.
    pub fn resume(&self) {
        if let Some(mut pursuit) = self.get()
            && !pursuit.is_complete
        {
            pursuit.terminal_reason = None;
            self.set(pursuit);
        }
        *lock(&self.armed) = true;
    }

    pub fn disarm(&self) {
        *lock(&self.armed) = false;
    }

    /// Stop the current execution attempt and retain its reason on the durable
    /// pursuit view. An earlier, more specific reason (for example a budget
    /// axis) wins over a later generic interruption.
    pub fn stop(&self, reason: impl Into<String>) -> Option<Pursuit> {
        let mut pursuit = self.get()?;
        if !pursuit.is_complete && pursuit.terminal_reason.is_none() {
            pursuit.terminal_reason = Some(reason.into());
            self.set(pursuit.clone());
        }
        self.disarm();
        Some(pursuit)
    }

    pub fn is_armed(&self) -> bool {
        *lock(&self.armed)
    }

    pub fn iterations(&self) -> u32 {
        *lock(&self.iterations)
    }

    /// Restore the stop-gate runtime view from persisted state on resume
    /// (ADR-0048 Phase 2). Unlike `arm`, this does NOT reset the iteration
    /// counter — an armed pursuit mid-iteration resumes with its count intact
    /// instead of starting over.
    pub fn restore_runtime(&self, armed: bool, iterations: u32, stats: PursuitStats) {
        *lock(&self.armed) = armed;
        *lock(&self.iterations) = iterations;
        *lock(&self.stats) = stats;
    }

    /// Increment the continuation counter each time the stop-gate forces
    /// another ReAct turn within the current round.
    pub fn bump_iterations(&self) {
        *lock(&self.iterations) += 1;
    }

    /// Book one pass's token usage and duration into the pursuit's running
    /// stats (ADR-0083). Called at a stop-gate boundary, before continuation
    /// policy, so the budget check sees up-to-date totals. No-op when no
    /// pursuit is armed.
    pub(crate) fn book_pass(&self, usage: neenee_core::TokenUsage, duration_ms: u64) {
        if self.is_armed() {
            lock(&self.stats).book_pass(usage, duration_ms);
        }
    }

    /// A snapshot of the per-pursuit runtime counters (passes/tokens/wall-clock).
    pub fn stats(&self) -> PursuitStats {
        *lock(&self.stats)
    }

    // ── continuation logic ──────────────────────────────────────────────

    /// Returns a continuation prompt to force another ReAct turn, or `None`
    /// to let the round end. Consulted by both execution paths just before
    /// they return `RoundOutcome`.
    ///
    /// Returns `Some(prompt)` only when: the gate is armed, an active
    /// (incomplete) pursuit exists, the latest response did not signal
    /// completion (via the marker), the iteration cap is not exhausted, AND no
    /// configured budget has been exceeded.
    ///
    /// Hitting the iteration cap or a budget disarms the pursuit and stamps a
    /// terminal reason (so the completion message can name the cause).
    pub(crate) fn continuation(
        &self,
        response: &neenee_core::Message,
        max_iterations: u32,
    ) -> Option<String> {
        if !self.is_armed() {
            return None;
        }
        let pursuit = self.get()?;
        if pursuit.is_complete {
            return None;
        }
        if response.content.contains(crate::PURSUIT_COMPLETE_MARKER) {
            return None;
        }
        let stats = self.stats();
        // Budget hard-stop (ADR-0069): if any configured budget is reached,
        // stamp a terminal reason, persist it, and stop.
        if let Some(budget) = pursuit.budget
            && let Some(reason) =
                budget.exceeded_reason(stats.passes, stats.tokens, stats.wall_clock_ms)
        {
            self.stop(reason);
            return None;
        }
        // `iterations` counts continuations already forced. The initial model
        // pass is iteration 1, so stop before another continuation would make
        // the total number of passes exceed the advertised cap.
        if self.iterations().saturating_add(1) >= max_iterations {
            self.stop(format!(
                "safety cap reached ({max_iterations} pursuit passes)"
            ));
            return None;
        }
        Some(pursuit_prompts::continuation_prompt(&pursuit))
    }

    /// Append a hidden user message that asks the model to continue the active pursuit.
    pub fn inject_continuation(&self, messages: &mut Vec<neenee_core::Message>) {
        if let Some(pursuit) = self.get()
            && !pursuit.is_complete
        {
            messages.push(crate::conversation_context::hidden_user(
                neenee_core::InjectionKind::PursuitContinuation,
                pursuit_prompts::continuation_prompt(&pursuit),
            ));
        }
    }

    /// Append a hidden user message that informs the model the pursuit objective changed.
    pub fn inject_objective_updated(&self, messages: &mut Vec<neenee_core::Message>) {
        if let Some(pursuit) = self.get() {
            messages.push(crate::conversation_context::hidden_user(
                neenee_core::InjectionKind::PursuitObjectiveUpdated,
                pursuit_prompts::objective_updated_prompt(&pursuit),
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use neenee_core::{Message, PursuitBudget, Role};

    fn response() -> Message {
        Message::new(Role::Assistant, "still working")
    }

    #[test]
    fn arm_clears_the_previous_attempt_reason() {
        let state = PursuitState::new();
        let mut pursuit = Pursuit::new("ship");
        pursuit.terminal_reason = Some("interrupted".to_string());
        state.set(pursuit);

        state.arm();

        assert!(state.is_armed());
        assert!(state.get().unwrap().terminal_reason.is_none());
    }

    #[test]
    fn resume_preserves_restored_counters() {
        let state = PursuitState::new();
        state.set(Pursuit::new("ship"));
        state.restore_runtime(
            true,
            7,
            PursuitStats {
                passes: 7,
                tokens: 42_000,
                wall_clock_ms: 5_000,
            },
        );

        state.resume();

        assert!(state.is_armed());
        assert_eq!(state.iterations(), 7);
        assert_eq!(state.stats().passes, 7);
        assert_eq!(state.stats().tokens, 42_000);
        assert_eq!(state.stats().wall_clock_ms, 5_000);
    }

    #[test]
    fn safety_cap_stops_with_an_explicit_reason() {
        let state = PursuitState::new();
        state.set(Pursuit::new("ship"));
        state.arm();
        for _ in 0..4 {
            assert!(state.continuation(&response(), 5).is_some());
            state.bump_iterations();
        }

        assert!(state.continuation(&response(), 5).is_none());
        assert!(!state.is_armed());
        assert_eq!(
            state.get().unwrap().terminal_reason.as_deref(),
            Some("safety cap reached (5 pursuit passes)")
        );
    }

    #[test]
    fn budget_stop_preserves_the_specific_axis_reason() {
        let state = PursuitState::new();
        let mut pursuit = Pursuit::new("ship");
        pursuit.budget = Some(PursuitBudget {
            max_passes: Some(1),
            ..Default::default()
        });
        state.set(pursuit);
        state.arm();
        lock(&state.stats).passes = 1;

        assert!(state.continuation(&response(), 50).is_none());
        assert_eq!(
            state.get().unwrap().terminal_reason.as_deref(),
            Some("pursuit pass budget reached (1/1)")
        );
    }

    #[test]
    fn generic_stop_does_not_overwrite_a_budget_reason() {
        let state = PursuitState::new();
        let mut pursuit = Pursuit::new("ship");
        pursuit.terminal_reason = Some("token budget reached".to_string());
        state.set(pursuit);
        state.arm();
        state.stop("interrupted");

        // `arm` starts a fresh attempt and therefore clears the old reason;
        // once the attempt has a reason, later generic stops retain it.
        state.stop("superseded");
        assert_eq!(
            state.get().unwrap().terminal_reason.as_deref(),
            Some("interrupted")
        );
    }
}
