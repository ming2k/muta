//! Authorized SystemWake continuation (ADR-0234 Stage 2b).
//!
//! A settled background job publishes a task event. That event does *not* by
//! itself start a model round: starting one spends inference and is autonomous
//! behavior, so it requires authorization. This module owns the two decisions
//! standing between a settle and a continuation round, and nothing else — the
//! driver owns when to ask, the manager owns the result.
//!
//! 1. **Authority.** Only a session whose human channel is already declared
//!    autonomous may continue on its own ([`ContinuationAuthority`], read from
//!    the agent's unattended posture). An interactive session is passive *by
//!    construction*: the human is present and will ask. Dispatching work to the
//!    background is never itself an authorization (INV-BG-03).
//! 2. **Budget.** Authority is not a license to self-drive without bound: one
//!    originating request buys a finite number of wake rounds, and a wake round
//!    does not re-arm that budget (INV-BG-04/06). The human's next request does.
//!
//! Both decisions are deliberately *pure* so they can be tested without a live
//! agent, provider, or job fabric.

use muta_contracts::{BackgroundJobOutcome, JobState};

/// A request from the task fabric for one authorized continuation round.
///
/// Carries only the session identity: the driver claims that session's retained
/// outcomes itself, so the result it reasons about is the one it acknowledged
/// (ADR-0234). This rides a dedicated channel, never [`AgentRequest`], so
/// machine-generated work cannot be mistaken for human intent (INV-BG-04).
#[derive(Debug, Clone)]
pub struct SystemWake {
    /// The session whose dispatched work settled.
    pub session_id: String,
}

/// Outcome of admitting a wake request, for tests and tracing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WakeAdmission {
    /// A continuation round was started.
    Started,
    /// The session's posture does not authorize autonomous rounds.
    NotAuthorized,
    /// The originating request's wake budget is spent.
    BudgetExhausted,
    /// Human follow-up work is queued; the human goes first.
    DeferredToHuman,
    /// There was nothing left to deliver (already collected, or none retained).
    NothingPending,
}

impl WakeAdmission {
    /// Whether the caller should treat the wake as consumed.
    pub fn consumed(self) -> bool {
        matches!(self, Self::Started | Self::NothingPending)
    }
}

/// Wake rounds one originating request may start by default.
///
/// One is the conservative floor: the model sees the result, takes one
/// follow-up action, and stops. A longer autonomous chain (run tests → fix →
/// run again) is a deliberate, larger grant, never a default.
pub const DEFAULT_WAKE_BUDGET: u32 = 1;

/// Upper bound on a harness-authored continuation digest, in bytes.
///
/// The digest rides in the model's context, so it is bounded like any other
/// injected content; the full output stays reachable through the `process`
/// tool.
pub const MAX_DIGEST_BYTES: usize = 4_096;

/// Whether a session may start rounds on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContinuationAuthority {
    /// No autonomous continuation. The default for an interactive session,
    /// where a human is attached and will ask for follow-up themselves.
    Passive,
    /// Bounded autonomous continuation, authorized by the session's declared
    /// unattended posture (ADR-0132's persisted posture).
    Unattended,
}

impl ContinuationAuthority {
    /// Read the authority from the session's declared posture.
    ///
    /// The unattended posture already means "never wait for confirmations,
    /// questions, or stdin" — a session that has declared no human is present.
    /// That is exactly the condition under which continuing without a prompt is
    /// meaningful, so it is reused rather than adding a second configuration
    /// face that could disagree with it.
    pub fn of(unattended: bool) -> Self {
        if unattended {
            Self::Unattended
        } else {
            Self::Passive
        }
    }

    /// Whether this authority permits a wake round at all.
    pub fn permits_wake(self) -> bool {
        matches!(self, Self::Unattended)
    }
}

/// The continuation budget for one originating request.
///
/// The budget is consumed *before* the wake round starts, so no failure mode —
/// including a crash mid-round or a mistake in the caller — can turn a single
/// grant into a loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WakeBudget {
    authority: ContinuationAuthority,
    remaining: u32,
}

impl WakeBudget {
    /// A budget for a session in the given posture.
    pub fn new(unattended: bool) -> Self {
        let authority = ContinuationAuthority::of(unattended);
        Self {
            authority,
            remaining: if authority.permits_wake() {
                DEFAULT_WAKE_BUDGET
            } else {
                0
            },
        }
    }

    /// A budget with an explicit size, for a caller configured with a larger
    /// grant than the default.
    pub fn with_limit(unattended: bool, limit: u32) -> Self {
        let authority = ContinuationAuthority::of(unattended);
        Self {
            authority,
            remaining: if authority.permits_wake() { limit } else { 0 },
        }
    }

    /// Re-arm the budget for a new originating request.
    ///
    /// Called when a *human/request-initiated* round is admitted. A wake round
    /// deliberately does not call this — that is what stops an autonomous chain
    /// from renewing its own authority.
    pub fn on_originating_request(&mut self) {
        self.remaining = if self.authority.permits_wake() {
            DEFAULT_WAKE_BUDGET
        } else {
            0
        };
    }

    /// Try to spend one wake round.
    ///
    /// Returns `true` exactly once per granted wake round. The counter is
    /// decremented *before* returning, so a caller that crashes after this call
    /// still loses the round rather than repeating it.
    pub fn try_claim(&mut self) -> bool {
        if !self.authority.permits_wake() || self.remaining == 0 {
            return false;
        }
        self.remaining -= 1;
        true
    }

    /// Wake rounds still available to the current originating request.
    pub fn remaining(&self) -> u32 {
        self.remaining
    }

    /// The authority this budget was created under.
    pub fn authority(&self) -> ContinuationAuthority {
        self.authority
    }
}

/// Short, settled-state rendering for a continuation digest.
fn state_label(state: &JobState) -> String {
    match state {
        JobState::Succeeded { exit_code, .. } => format!("succeeded (exit {exit_code})"),
        JobState::Failed {
            exit_code, error, ..
        } => format!("failed (exit {exit_code}): {error}"),
        JobState::Killed { .. } => "was cancelled".to_string(),
        JobState::TimedOut { .. } => "timed out".to_string(),
        JobState::Queued => "is queued".to_string(),
        JobState::Running { .. } => "is running".to_string(),
        JobState::Ready { .. } => "is ready".to_string(),
    }
}

/// Build the harness-authored digest describing settled background work.
///
/// The digest is explicitly labelled as a harness report so the model does not
/// mistake it for a human instruction, and it is bounded: the complete output
/// stays behind the `process` tool rather than being poured into context.
pub fn continuation_digest(outcomes: &[BackgroundJobOutcome]) -> String {
    let mut body = String::new();
    for outcome in outcomes {
        let label = crate::task_digest::job_label(&outcome.spec);
        body.push_str(&format!(
            "- `{}` ({label}) {}\n",
            outcome.job_id.0,
            state_label(&outcome.state)
        ));
        let summary = outcome.summary.trim();
        if !summary.is_empty() {
            for line in summary.lines() {
                body.push_str(&format!("    {line}\n"));
            }
        }
        if let Some(path) = &outcome.log_path {
            body.push_str(&format!("    full log: {}\n", path.display()));
        }
    }

    let header = format!(
        "[background task report — harness-authored, not a user message. \
         {} task(s) you dispatched have finished:",
        outcomes.len()
    );
    let footer = "Inspect any job in full with the process tool (action: 'status' or 'logs').]";

    let body = muta_contracts::tool_output::truncate_utf8(
        &body,
        MAX_DIGEST_BYTES.saturating_sub(header.len() + footer.len() + 2),
    );
    format!("{header}\n{body}\n{footer}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outcome(id: &str, state: JobState, summary: &str) -> BackgroundJobOutcome {
        BackgroundJobOutcome {
            job_id: muta_contracts::JobId(id.to_string()),
            spec: muta_contracts::JobSpec::Process {
                command: "cargo nextest run".to_string(),
                label: Some("cargo-test".to_string()),
                cwd: None,
                detached: false,
                task_kind: muta_contracts::JobKind::Interactive,
                readiness: None,
                restart: None,
            },
            state,
            summary: summary.to_string(),
            log_path: None,
        }
    }

    #[test]
    fn interactive_sessions_are_passive_by_construction() {
        // The default posture must never wake: a human is attached and will
        // ask. This is the property that keeps the feature inert for every
        // existing interactive session.
        let mut budget = WakeBudget::new(false);
        assert_eq!(budget.authority(), ContinuationAuthority::Passive);
        assert!(
            !budget.try_claim(),
            "an interactive session must never wake"
        );
        assert!(!budget.try_claim());
        budget.on_originating_request();
        assert!(
            !budget.try_claim(),
            "re-arming must not create authority that never existed"
        );
    }

    #[test]
    fn an_unattended_session_gets_one_wake_per_originating_request() {
        let mut budget = WakeBudget::new(true);
        assert!(budget.try_claim(), "the first wake is granted");
        assert!(!budget.try_claim(), "the default grant is one round");
        assert_eq!(budget.remaining(), 0);

        // A human/request-initiated round re-arms it.
        budget.on_originating_request();
        assert!(budget.try_claim());
    }

    /// The safety property that makes an unbounded loop impossible: a wake
    /// round cannot renew its own authority.
    #[test]
    fn a_wake_round_does_not_re_arm_the_budget() {
        let mut budget = WakeBudget::new(true);
        assert!(budget.try_claim());
        // Simulate the wake round running to completion and its job settling.
        // Nothing in that path calls `on_originating_request`, so the next
        // attempt must fail however many times it is asked.
        for _ in 0..10 {
            assert!(!budget.try_claim(), "the budget must stay spent");
        }
        assert_eq!(budget.remaining(), 0);
    }

    #[test]
    fn the_claim_is_consumed_before_the_round_runs() {
        // A crash after `try_claim` must lose the round, never repeat it: the
        // counter is already spent when the caller receives `true`.
        let mut budget = WakeBudget::new(true);
        assert!(budget.try_claim());
        let resumed_after_crash = budget;
        assert_eq!(resumed_after_crash.remaining(), 0);
        assert!(
            !budget.try_claim(),
            "the round is not available a second time"
        );
    }

    #[test]
    fn an_explicit_larger_grant_is_honoured() {
        let mut budget = WakeBudget::with_limit(true, 3);
        assert_eq!(budget.remaining(), 3);
        assert!(budget.try_claim());
        assert!(budget.try_claim());
        assert!(budget.try_claim());
        assert!(!budget.try_claim());
    }

    #[test]
    fn a_passive_session_cannot_be_given_a_grant_by_raising_the_limit() {
        let mut budget = WakeBudget::with_limit(false, 5);
        assert_eq!(budget.remaining(), 0);
        assert!(!budget.try_claim());
    }

    #[test]
    fn the_digest_names_the_job_state_and_result_and_labels_itself() {
        let digest = continuation_digest(&[outcome(
            "job_1",
            JobState::Failed {
                duration_ms: 1_200,
                exit_code: 101,
                error: "test failed".to_string(),
            },
            "assertion failed at src/lib.rs:42",
        )]);

        assert!(
            digest.contains("harness-authored, not a user message"),
            "the model must not mistake this for a human instruction: {digest}"
        );
        assert!(digest.contains("job_1"), "{digest}");
        assert!(
            digest.contains("cargo-test"),
            "the label identifies it: {digest}"
        );
        assert!(digest.contains("failed (exit 101)"), "{digest}");
        assert!(
            digest.contains("assertion failed at src/lib.rs:42"),
            "{digest}"
        );
        assert!(
            digest.contains("process tool"),
            "points at the full output: {digest}"
        );
    }

    #[test]
    fn the_digest_is_bounded_even_for_a_huge_summary() {
        let huge = "x".repeat(MAX_DIGEST_BYTES * 3);
        let digest = continuation_digest(&[outcome(
            "job_big",
            JobState::Succeeded {
                duration_ms: 5,
                exit_code: 0,
            },
            &huge,
        )]);
        assert!(
            digest.len() <= MAX_DIGEST_BYTES + 64,
            "the digest must stay bounded, was {}",
            digest.len()
        );
    }

    #[test]
    fn the_digest_covers_every_settled_job_in_the_batch() {
        let digest = continuation_digest(&[
            outcome(
                "job_a",
                JobState::Succeeded {
                    duration_ms: 1,
                    exit_code: 0,
                },
                "ok",
            ),
            outcome("job_b", JobState::Killed { duration_ms: 2 }, ""),
        ]);
        assert!(digest.contains("2 task(s)"), "{digest}");
        assert!(
            digest.contains("job_a") && digest.contains("job_b"),
            "{digest}"
        );
        assert!(digest.contains("was cancelled"), "{digest}");
    }
}
