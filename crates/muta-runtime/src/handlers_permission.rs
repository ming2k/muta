//! Permission / interruption handlers, extracted verbatim from the agent
//! background task's `match req { … }` dispatch.
//!
//! Each handler is one match arm, lifted unchanged. Parameters are named to
//! match the original loop locals (`agent`, `session`, `resp_tx`,
//! `lifecycle`, `side`, `runner_registry`, …) so the body reads exactly as
//! it did inline.

use muta_agent::orchestration::send_harness_state;
use muta_agent::{Agent, RunnerRegistry, RoundLifecycle};
use muta_contracts::{AgentResponse, LoopStatus, PermissionDecision};
use muta_persistence::session::SessionStore;
use std::sync::Arc;
use tokio::sync::{RwLock as AsyncRwLock, mpsc};

/// Option labels of the workspace-trust question (see `serve.rs`'s
/// `workspace_trust_prompt` and the `/trust` flows). The reply handler
/// matches these exact tokens so prompt copy and semantics cannot drift
/// apart. *Trust* answers decide *whether* authority is granted at all;
/// *development* is merely the profile one of the grants selects.
pub(crate) const SECURITY_TRUST_OPTION_FULL: &str = "Grant development authority (with extensions)";
pub(crate) const SECURITY_TRUST_OPTION_WORKSPACE_ONLY: &str =
    "Grant development authority (workspace only)";
pub(crate) const SECURITY_TRUST_OPTION_RESTRICTED: &str = "Keep restricted";

/// Apply a trust-prompt answer to the durable security store. Structured
/// tokens, not substring matching: the option labels are exactly the three
/// published by the trust prompt (`serve.rs`'s attach push and the `/trust`
/// flows). Matching on substrings like "Development" let this handler's
/// semantics drift with every copy edit of the prompt. An unrecognised
/// answer (legacy client, hand-crafted reply) fails closed to restricted.
pub fn apply_trust_decision(
    workspace_security: &muta_persistence::workspace_security::WorkspaceSecurityStore,
    project_root: &std::path::Path,
    chosen: &str,
) -> &'static str {
    match chosen {
        SECURITY_TRUST_OPTION_FULL => {
            let _ = workspace_security.set_execution(
                project_root,
                muta_contracts::WorkspaceExecutionProfile::Development,
            );
            let _ = workspace_security.trust_extensions(project_root);
            "✓ Development authority granted and project extensions trusted. Decision persisted."
        }
        SECURITY_TRUST_OPTION_WORKSPACE_ONLY => {
            let _ = workspace_security.set_execution(
                project_root,
                muta_contracts::WorkspaceExecutionProfile::Development,
            );
            let _ = workspace_security.untrust_extensions(project_root);
            "✓ Development authority granted; project extensions stay quarantined. Decision persisted."
        }
        // Restricted, and any unrecognised token: least privilege.
        _ => {
            let _ = workspace_security.set_execution(
                project_root,
                muta_contracts::WorkspaceExecutionProfile::Restricted,
            );
            let _ = workspace_security.untrust_extensions(project_root);
            "✓ Workspace restricted to read-oriented operations. Decision persisted."
        }
    }
}

use crate::side::SideRegistry;

/// `AgentRequest::Interrupt` — reject every pending permission, question, and
/// interactive-input waiter; flip the harness to idle eagerly (before the
/// in-flight round's own terminal
/// idle snapshot, which is gated behind persistence fsyncs), then cancel the
/// live token. The generation is deliberately NOT bumped
/// ([`RoundLifecycle::cancel_current`], not `supersede`) so the stale round
/// still emits its own "... \[Interrupted\]" cleanup.
pub async fn interrupt(
    agent: &Agent,
    session: &Arc<SessionStore>,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    lifecycle: &Arc<RoundLifecycle>,
) {
    // Park the reason before anything else so the unwinding round's tail can
    // label its own terminal event + durable record (C11): this stop is the
    // user's explicit Esc Esc (or the control-plane Interrupt equivalent).
    lifecycle.record_interrupt(muta_contracts::RoundInterruptReason::User);
    agent.reject_pending_permissions();
    agent.reject_pending_user_questions();
    agent.reject_pending_inputs();
    let _ = resp_tx.send(AgentResponse::PermissionsCleared);

    // Flip the harness to idle the instant interrupt is requested — BEFORE
    // the in-flight turn unwinds. The work itself stops the moment the token
    // is cancelled below, but the round task's own terminal "idle" snapshot is
    // only sent at the very end of its cleanup, which is gated behind
    // persistence fsyncs (`session.replace_messages` inside `execute_round`,
    // then `set_checkpoint` in `start_pursuit`). Without this eager snapshot
    // the activity bar keeps showing the stale "pursue"/"running" loop_status
    // — and a climbing elapsed timer — for the whole disk-write window, which
    // reads as "still working" when the work is already stopped.
    //
    // This is idempotent with the stale task's later idle send: if no new
    // round starts, both snapshots are "idle"; if one does, it bumps
    // generation itself and its "running" snapshot supersedes, while the
    // stale task's generation-guarded idle send is skipped
    // (`orchestration.rs` start_pursuit / start_interactive_round /
    // run_shell_command).
    send_harness_state(resp_tx, &session.id().await, agent, LoopStatus::Idle);

    lifecycle.cancel_current().await;
}

/// `AgentRequest::PermissionReply` — full-duplex routing (ADR-0029): a reply
/// tagged with a `parent_call_id` targets an runner's parked oneshot via the
/// registry handle; `None` keeps the legacy top-level (/btw side) path. A late
/// reply after the child finished finds no handle and falls through to the
/// "no longer pending" error.
pub async fn reply(
    agent: &Agent,
    runner_registry: &Arc<RunnerRegistry>,
    side: &Arc<AsyncRwLock<SideRegistry>>,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    request_id: String,
    decision: PermissionDecision,
    parent_call_id: Option<String>,
) {
    // Three-level routing, mirroring `reply_question` / `reply_input`: a
    // `parent_call_id` targets an runner; otherwise the primary, then a
    // `/btw` side agent. (The side fallback was missing here — a side
    // agent's permission banner reply used to fall through to "no longer
    // pending" and park forever. ADR-0141 makes all three reply handlers
    // uniform.)
    let resolved = if let Some(parent) = &parent_call_id {
        runner_registry
            .get(parent)
            .is_some_and(|handle| handle.reply_permission(&request_id, decision))
    } else if agent.reply_permission(&request_id, decision) {
        true
    } else {
        let mut routed = false;
        for s in side.read().await.iter() {
            if s.agent.reply_permission(&request_id, decision) {
                routed = true;
                break;
            }
        }
        routed
    };
    if !resolved {
        let _ = resp_tx.send(AgentResponse::Error(
            "Permission request is no longer pending.".to_string(),
        ));
    }
}

/// `AgentRequest::UserQuestionReply` — mirror the permission arm: a
/// `parent_call_id` targets the runner; otherwise try the primary, then a
/// `/btw` side agent (ADR-0017).
#[allow(clippy::too_many_arguments)]
pub async fn reply_question(
    agent: &Agent,
    runner_registry: &Arc<RunnerRegistry>,
    side: &Arc<AsyncRwLock<SideRegistry>>,
    workspace_security: &Arc<muta_persistence::workspace_security::WorkspaceSecurityStore>,
    project_root: &std::path::Path,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    session_id: &str,
    request_id: String,
    answers: Vec<Vec<String>>,
    parent_call_id: Option<String>,
) {
    if request_id.starts_with("trust-") {
        let chosen = answers
            .first()
            .and_then(|row| row.first())
            .map(|s| s.as_str())
            .unwrap_or(SECURITY_TRUST_OPTION_RESTRICTED);
        let msg = apply_trust_decision(workspace_security, project_root, chosen);

        let snapshot = workspace_security.snapshot(project_root);
        agent.set_workspace_security(snapshot);
        let _ = resp_tx.send(muta_agent::orchestration::round_response(
            session_id,
            muta_contracts::RoundEvent::Notice(
                muta_contracts::AgentNotice::new(
                    muta_contracts::NoticeKind::ReviewAlert,
                    muta_contracts::NoticeSeverity::Info,
                    "Workspace trust decision recorded",
                    muta_contracts::NoticeSource::Harness,
                )
                .with_surface(muta_contracts::NoticeSurface::Toast)
                .with_body(msg),
            ),
        ));
        send_harness_state(resp_tx, session_id, agent, LoopStatus::Idle);
        return;
    }

    let resolved = if let Some(parent) = &parent_call_id {
        runner_registry
            .get(parent)
            .is_some_and(|handle| handle.reply_user_question(&request_id, answers.clone()))
    } else if agent.reply_user_question(&request_id, answers.clone()) {
        true
    } else {
        let mut routed = false;
        for s in side.read().await.iter() {
            if s.agent.reply_user_question(&request_id, answers.clone()) {
                routed = true;
                break;
            }
        }
        routed
    };
    if !resolved {
        let _ = resp_tx.send(AgentResponse::Error(
            "Question request is no longer pending.".to_string(),
        ));
    }
}

/// `AgentRequest::InputReply` (L3.5 β) — mirrors [`reply_question`]: a
/// `parent_call_id` targets the runner; otherwise try the primary, then a
/// `/btw` side agent.
pub async fn reply_input(
    agent: &Agent,
    runner_registry: &Arc<RunnerRegistry>,
    side: &Arc<AsyncRwLock<SideRegistry>>,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    request_id: String,
    text: String,
    parent_call_id: Option<String>,
) {
    let resolved = if let Some(parent) = &parent_call_id {
        runner_registry
            .get(parent)
            .is_some_and(|handle| handle.reply_input(&request_id, text.clone()))
    } else if agent.reply_input(&request_id, text.clone()) {
        true
    } else {
        let mut routed = false;
        for s in side.read().await.iter() {
            if s.agent.reply_input(&request_id, text.clone()) {
                routed = true;
                break;
            }
        }
        routed
    };
    if !resolved {
        let _ = resp_tx.send(AgentResponse::Error(
            "Input request is no longer pending.".to_string(),
        ));
    }
}
