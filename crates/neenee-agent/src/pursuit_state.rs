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

/// Runtime counters accumulated against an armed pursuit (ADR-0069).
///
/// These are session-scoped: they start at zero when a pursuit is armed and are
/// not part of the durable pursuit record (they are rebuilt on resume from the
/// persisted `iterations`). They exist so the [`PursuitBudget`] can be enforced
/// and a convergence reminder can fire past 75%.
#[derive(Debug, Clone, Copy, Default)]
pub struct PursuitStats {
    /// Continuation turns driven by the stop-gate (mirrors `iterations`).
    pub turns: u32,
    /// Cumulative tokens (prompt + completion) across all pursuit turns.
    pub tokens: u64,
    /// Wall-clock milliseconds spent in `active` turns.
    pub wall_clock_ms: u64,
}

impl PursuitStats {
    /// Book one turn's usage into the running totals.
    fn book_turn(&mut self, usage: neenee_core::TokenUsage, duration_ms: u64) {
        self.turns += 1;
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
    /// Per-pursuit runtime counters (turns/tokens/wall-clock). Reset on arm.
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
    pub fn arm(&self) {
        *lock(&self.iterations) = 0;
        *lock(&self.stats) = PursuitStats::default();
        *lock(&self.armed) = true;
    }

    pub fn disarm(&self) {
        *lock(&self.armed) = false;
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
    pub fn restore_runtime(&self, armed: bool, iterations: u32) {
        *lock(&self.armed) = armed;
        *lock(&self.iterations) = iterations;
    }

    /// Increment the iteration counter (called by the turn loops each time
    /// the stop-gate forces another round).
    pub fn bump_iterations(&self) {
        *lock(&self.iterations) += 1;
    }

    /// Book one turn's token usage + duration into the pursuit's running stats
    /// (ADR-0069). Called from the turn loop after a pursuit turn completes, so
    /// the budget check sees up-to-date totals. No-op when no pursuit is armed.
    pub(crate) fn book_turn(&self, usage: neenee_core::TokenUsage, duration_ms: u64) {
        if self.is_armed() {
            lock(&self.stats).book_turn(usage, duration_ms);
        }
    }

    /// A snapshot of the per-pursuit runtime counters (turns/tokens/wall-clock).
    pub fn stats(&self) -> PursuitStats {
        *lock(&self.stats)
    }

    // ── continuation logic ──────────────────────────────────────────────

    /// Returns a continuation prompt to force another model round, or `None`
    /// to let the turn end. Consulted by both turn loops just before they
    /// return `RoundOutcome`.
    ///
    /// Returns `Some(prompt)` only when: the gate is armed, an active
    /// (incomplete) pursuit exists, the latest response did not signal
    /// completion (via the marker), the iteration cap is not exhausted, AND no
    /// configured budget has been exceeded.
    ///
    /// Hitting the iteration cap or a budget disarms the pursuit and stops. A
    /// budget exceedance stamps a terminal reason on the pursuit (so the
    /// completion message can name the cause).
    pub(crate) fn continuation(
        &self,
        response: &neenee_core::Message,
        max_iterations: u32,
    ) -> Option<String> {
        if !self.is_armed() {
            return None;
        }
        let mut pursuit = self.get()?;
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
                budget.exceeded_reason(stats.turns, stats.tokens, stats.wall_clock_ms)
        {
            pursuit.terminal_reason = Some(reason);
            self.set(pursuit);
            self.disarm();
            return None;
        }
        if self.iterations() >= max_iterations {
            self.disarm();
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
