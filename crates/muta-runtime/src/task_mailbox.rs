//! ADR-0190 mailbox: task-fabric completion events are classified here and, when
//! the session's posture authorizes it, offered to the driver as a continuation.
//!
//! This task is the fabric arm of the driver loop, running as its own task so
//! wake requests contend with user input on equal terms instead of blocking the
//! request channel.
//!
//! Two boundaries are enforced here rather than in the driver:
//!
//! - **Classification.** Whether a settle is wake-eligible at all (an operator
//!   cancellation is not; a service reporting readiness is not a completion).
//! - **Vehicle.** A wake rides a dedicated [`SystemWake`](crate::task_continuation::SystemWake)
//!   channel. ADR-0212 removed the shortcut of routing machine outcomes through
//!   the human follow-up queue, and that boundary stands: a machine result must
//!   never be smuggled in as [`AgentRequest::FollowUp`]. Whether a wake is
//!   *admitted* is the driver's decision, because only the driver knows the
//!   round state, the authorization, and whether a human is waiting.

use std::sync::Arc;

use muta_contracts::{BackgroundJobOutcome, JobSpec, JobState};
use tokio::sync::RwLock;

use crate::side::SideRegistry;
use crate::task_continuation::SystemWake;

/// Environment handle the mailbox task owns (cloned Arcs, not borrows —
/// this task outlives a single driver-loop iteration).
#[allow(dead_code)]
pub(crate) struct MailboxEnv {
    pub side: Arc<RwLock<SideRegistry>>,
    pub agent: Arc<muta_agent::Agent>,
    pub session: Arc<muta_persistence::session::SessionStore>,
    pub lifecycle: Arc<muta_agent::RoundLifecycle>,
    /// Outbound responses (notices, state) to the frontend.
    pub tx: ResponseTx,
    /// Inbound request channel for driver-owned dispatch.
    pub req_tx: RequestTx,
    /// Dedicated continuing-work channel (ADR-0234). Separate from `req_tx` by
    /// design: a machine result is not a human request.
    pub wake_tx: WakeTx,
    pub config: muta_persistence::config::Config,
}

pub type ResponseTx = tokio::sync::mpsc::UnboundedSender<muta_contracts::AgentResponse>;
pub type RequestTx = tokio::sync::mpsc::Sender<muta_contracts::AgentRequest>;
pub type WakeTx = tokio::sync::mpsc::Sender<SystemWake>;

/// Classify a fabric event for wake eligibility (ADR-0190 D3).
pub(crate) enum FabricWake {
    /// Offer a continuation for this settlement.
    Wake { digest: String },
    /// UI-only event — never a continuation.
    Silent,
}

/// Decide whether a completion outcome is worth offering as a continuation, and
/// with what digest.
///
/// `Interactive` tasks are eligible on settle. A `Service` that an operator
/// deliberately killed is silent — the operator's `kill` was the answer, so
/// waking to report it would spend inference restating a decision.
pub(crate) fn classify_outcome(outcome: &BackgroundJobOutcome) -> FabricWake {
    let is_service = matches!(
        &outcome.spec,
        JobSpec::Process {
            task_kind: muta_contracts::JobKind::Service,
            ..
        }
    );
    let killed_by_operator = matches!(outcome.state, JobState::Killed { .. });
    if is_service && killed_by_operator {
        // A deliberate stop needs no model turn.
        return FabricWake::Silent;
    }
    FabricWake::Wake {
        digest: crate::task_digest::outcome_digest(outcome),
    }
}

/// Offer a settled result to the driver for a possible continuation round
/// (ADR-0234).
///
/// The digest is not passed on: the driver claims the session's retained
/// outcomes and reports exactly what it acknowledged, so the model's context and
/// the delivery record cannot disagree. A closed driver channel is a normal
/// shutdown, not an error — the result stays retained for the session's
/// lifetime.
pub(crate) async fn request_wake_turn(env: &MailboxEnv, session_id: &str) {
    let _ = env.wake_tx.send(SystemWake {
        session_id: session_id.to_string(),
    });
}

/// Consume the manager's event stream and offer eligible settlements to the
/// driver. Runs for the life of the session.
pub(crate) async fn run_mailbox(
    mut events: tokio::sync::broadcast::Receiver<crate::background_jobs::BackgroundJobEvent>,
    env: MailboxEnv,
    session_id: String,
) {
    loop {
        match events.recv().await {
            Ok(crate::background_jobs::BackgroundJobEvent::Completed(outcome)) => {
                if let FabricWake::Wake { digest: _ } = classify_outcome(&outcome) {
                    request_wake_turn(&env, &session_id).await;
                }
            }
            // Readiness and progress are task-bar facts, not results: the model
            // is not woken to be told a server is up. It can inspect any job
            // with the `process` tool on its own initiative.
            Ok(_) => {}
            // A lagged subscriber must not stop the mailbox: skip the gap. The
            // settlements it missed are still retained by the manager, so the
            // next wake carrying them loses nothing.
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
            Err(_) => break,
        }
    }
}
