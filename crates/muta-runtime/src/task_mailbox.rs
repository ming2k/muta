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

use muta_agent::orchestration::{RoundDriver, RoundInput};
use muta_contracts::{BackgroundJobOutcome, JobSpec, JobState};
use tokio::sync::RwLock;

use crate::side::SideRegistry;
use crate::task_digest::outcome_digest;

/// Environment handle the mailbox task owns (cloned Arcs, not borrows —
/// this task outlives a single driver-loop iteration).
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
pub type RequestTx = tokio::sync::mpsc::UnboundedSender<muta_contracts::AgentRequest>;

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

/// Queue a wake turn. If a round is active the digest is delivered at the
/// round boundary (queued follow-up, ADR-0126); if idle it starts a fresh
/// round immediately. This is the mailbox arm's action.
pub(crate) async fn request_wake_turn(env: &MailboxEnv, session_id: &str, digest: String) {
    let running = env.lifecycle.is_running().await;
    if running {
        // Round-boundary delivery: dispatch through the request channel as a
        // follow-up so the existing queue machinery linearizes it (ADR-0126).
        let queued = muta_contracts::QueuedMessage {
            id: uuid::Uuid::new_v4().to_string(),
            text: digest,
            display_text: None,
            sent_at_ms: None,
            images: Vec::new(),
        };
        let _ = env.req_tx.send(muta_contracts::AgentRequest::FollowUp {
            session_id: session_id.to_string(),
            message: queued,
        });
    } else {
        let input = RoundInput {
            prompt: digest,
            hidden: false,
            display_prompt: Some("background task update".to_string()),
            sent_at_ms: None,
            images: Vec::new(),
            driver: RoundDriver::Fresh,
        };
        let _ = crate::side::start_session_turn(
            session_id,
            crate::side::SideEnv {
                side: &env.side,
                master: &env.agent,
                primary_session: &env.session,
                primary_lifecycle: &env.lifecycle,
                tx: &env.tx,
                config: &env.config,
            },
            input,
        )
        .await;
    }
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
