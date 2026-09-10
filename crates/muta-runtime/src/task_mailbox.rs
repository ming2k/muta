//! ADR-0190 mailbox: task-fabric completion events drive wake turns.
//!
//! The driver loop's fabric arm (D3): when a task settles (or a service
//! reports `Ready` / dies unsolicited), a harness-authored digest is queued
//! into the session and, at the next round boundary — immediately if the
//! session is idle — a **wake turn** starts so the model observes the
//! outcome. This closes the dead `outcome_receiver` loop: the tool
//! response's "you will receive an automatic notification" promise becomes a
//! wire-level invariant.

use std::sync::Arc;

use muta_contracts::{BackgroundJobOutcome, JobSpec, JobState};
use tokio::sync::RwLock;

use crate::side::SideRegistry;
use crate::task_digest::outcome_digest;

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
    /// Inbound request channel — wake turns dispatch as `FollowUp` requests
    /// through it so the existing queue machinery linearizes them (ADR-0126).
    pub req_tx: RequestTx,
    pub config: muta_persistence::config::Config,
}

pub type ResponseTx = tokio::sync::mpsc::UnboundedSender<muta_contracts::AgentResponse>;
pub type RequestTx = tokio::sync::mpsc::Sender<muta_contracts::AgentRequest>;

/// Classify a fabric event for wake eligibility (ADR-0190 D3).
pub(crate) enum FabricWake {
    /// Wake the model now (task settled, service crashed, service ready).
    Wake { digest: String },
    /// UI-only event — never wakes the model.
    Silent,
}

/// Decide whether a completion outcome should wake the session, and with
/// what digest. `Interactive` tasks always wake on settle (the foreground
/// sync path handed this task to the fabric); `Service` tasks wake on
/// unsolicited failure (crash awareness) but not on operator stop.
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
        // A deliberate `task_stop` needs no model turn.
        return FabricWake::Silent;
    }
    FabricWake::Wake {
        digest: outcome_digest(outcome),
    }
}

/// ADR-0212: FollowUpQueue belongs exclusively to authoritative human operator intent.
/// Machine-generated background digests MUST NOT be smuggled into the session request
/// channel as fake `AgentRequest::FollowUp` items.
///
/// Real-time execution status rides dedicated `BackgroundJobEvent` signals to the
/// client's TaskBar. Autonomous wakes, if ever re-enabled, must use a dedicated SystemWake
/// protocol and never preempt human follow-ups.
pub(crate) async fn request_wake_turn(_env: &MailboxEnv, _session_id: &str, _digest: String) {
    // Deliberately a no-op per ADR-0212: task outcomes update the TaskBar and do not
    // pollute user conversational history or follow-up queue.
}

/// Consume the manager's event stream and wake the session (the fabric arm
/// of the driver loop, running as its own task so wake requests contend
/// with user input on equal terms). Runs for the life of the session.
pub(crate) async fn run_mailbox(
    mut events: tokio::sync::broadcast::Receiver<crate::background_jobs::BackgroundJobEvent>,
    env: MailboxEnv,
    session_id: String,
) {
    loop {
        match events.recv().await {
            Ok(crate::background_jobs::BackgroundJobEvent::Completed(outcome)) => {
                if let FabricWake::Wake { digest } = classify_outcome(&outcome) {
                    request_wake_turn(&env, &session_id, digest).await;
                }
            }
            Ok(crate::background_jobs::BackgroundJobEvent::Ready { job_id }) => {
                let digest = format!(
                    "[background service ready: job `{}` is running and its \
                     readiness condition is met. Continue your work; use \
                     process_logs to inspect its output.]",
                    job_id.0
                );
                request_wake_turn(&env, &session_id, digest).await;
            }
            Ok(_) => {}
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
            Err(_) => break,
        }
    }
}
