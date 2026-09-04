//! Session lifecycle, creation, resumption, branching, and runtime teardown operations.

use std::sync::Arc;
use tokio::sync::{RwLock as AsyncRwLock, mpsc};

use super::SlashEnv;
use super::record::{record_command, record_error};
use crate::side::{SideRegistry, publish_btw_list};
use muta_agent::Agent;
use muta_agent::RoundLifecycle;
use muta_agent::orchestration::{round_response, send_harness_state_for_session};
use muta_contracts::{AgentNotice, AgentResponse, CommandResult, LoopStatus, RoundEvent};
use muta_persistence::config::Config;
use muta_persistence::session::SessionStore;

pub(crate) async fn supersede_for_session_switch(
    lifecycle: &RoundLifecycle,
    agent: &Agent,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
) {
    lifecycle.record_interrupt(muta_contracts::RoundInterruptReason::Superseded);
    lifecycle.supersede();
    agent.reject_pending_permissions();
    agent.reject_pending_user_questions();
    agent.reject_pending_inputs();
    let _ = resp_tx.send(AgentResponse::PermissionsCleared);
    lifecycle.cancel_current().await;
}

pub(crate) fn apply_additional_roots(
    handle: &muta_contracts::SharedAdditionalRoots,
    effective: &Config,
    project_root: &std::path::Path,
) {
    let resolved = effective
        .resolve_workspace_additional_roots(project_root)
        .unwrap_or_default();
    handle.store(resolved);
}

pub async fn teardown_sides_for_session_switch(
    side: &Arc<AsyncRwLock<SideRegistry>>,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
) {
    let ids: Vec<String> = side.read().await.iter().map(|s| s.id.clone()).collect();
    if ids.is_empty() {
        return;
    }
    let was_active = side.read().await.active().is_some();
    for id in ids {
        if let Some(s) = side.write().await.remove(&id) {
            s.lifecycle
                .record_interrupt(muta_contracts::RoundInterruptReason::Superseded);
            s.agent.reject_pending_permissions();
            s.agent.reject_pending_user_questions();
            s.agent.reject_pending_inputs();
            s.lifecycle.cancel_current().await;
            let _ = s.store.delete(&s.id).await;
        }
    }
    publish_btw_list(side, resp_tx).await;
    if was_active {
        let _ = resp_tx.send(AgentResponse::SideViewClosed);
    }
}

pub(crate) async fn start_fresh_session(env: &mut SlashEnv<'_>, name: &str, args: &str) {
    let (side, session, config, agent, lifecycle, resp_tx, provider_for_task, shared_unconfined) = (
        env.side,
        env.session,
        env.config,
        env.agent,
        env.lifecycle,
        env.resp_tx,
        env.provider_for_task,
        env.shared_unconfined,
    );
    let provider_usage = &mut *env.provider_usage;
    supersede_for_session_switch(lifecycle, agent, resp_tx).await;
    teardown_sides_for_session_switch(side, resp_tx).await;
    agent.clear_todos();
    match session.reset().await {
        Ok(id) => {
            let fresh_posture = session.delegated().await;
            if agent.delegated() != fresh_posture {
                agent.set_delegated(fresh_posture);
                let _ = resp_tx.send(round_response(
                    &id,
                    RoundEvent::DelegatedChanged(fresh_posture),
                ));
            }
            if shared_unconfined.is_unconfined() {
                shared_unconfined.set_unconfined(false);
                let _ = resp_tx.send(round_response(&id, RoundEvent::UnconfinedChanged(false)));
            }
            agent.restore_round_count(session.round_counter().await);
            crate::handlers_provider::reapply_session_selection(
                config,
                agent,
                provider_for_task,
                session,
                resp_tx,
                provider_usage,
            )
            .await;
            let _ = resp_tx.send(round_response(
                &session.id().await,
                RoundEvent::TodosUpdated(muta_contracts::TodoList::default()),
            ));
            let _ = resp_tx.send(AgentResponse::ConversationCleared {
                session_id: session.id().await,
            });
            record_command(
                session,
                resp_tx,
                name,
                args,
                CommandResult::Text(format!("Started new session: {}", id)),
            )
            .await;
        }
        Err(error) => {
            record_error(session, resp_tx, name, args, error).await;
        }
    }
}

pub(crate) async fn restore_session_runtime(
    session: &Arc<SessionStore>,
    agent: &Arc<Agent>,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    source: muta_contracts::SessionSource,
) {
    let todos = session.todos().await;
    agent.set_todos(todos.clone());
    let _ = resp_tx.send(round_response(
        &session.id().await,
        RoundEvent::TodosUpdated(todos),
    ));

    agent.restore_disabled_tools(session.disabled_tools().await);
    agent.restore_round_count(session.round_counter().await);

    let mut restored_delegated = session.delegated().await;
    let mut restored_from_ledger = false;
    if !restored_delegated {
        let commands = session.commands().await;
        let was_delegated_on = commands
            .iter()
            .rev()
            .find_map(|rec| {
                if (rec.name == "yolo" || rec.name == "autopilot" || rec.name == "delegate")
                    && let Some(CommandResult::Ack { title, .. }) = &rec.result
                {
                    let title = title.to_lowercase();
                    if title.contains("on") {
                        return Some(true);
                    } else if title.contains("off") {
                        return Some(false);
                    }
                }
                None
            })
            .unwrap_or(false);
        if was_delegated_on {
            restored_delegated = true;
            restored_from_ledger = true;
            let _ = session.set_delegated(true).await;
        }
    }

    if agent.delegated() != restored_delegated {
        agent.set_delegated(restored_delegated);
        if restored_from_ledger {
            let notice = AgentNotice::new(
                muta_contracts::NoticeKind::CommandAck,
                muta_contracts::NoticeSeverity::Warning,
                "Delegated mode restored",
                muta_contracts::NoticeSource::Harness,
            )
            .with_surface(muta_contracts::NoticeSurface::Inline)
            .with_body(
                "This session was previously running in delegated auto-approve mode. \
                 Use `/delegate off` to return to interactive mode.",
            );
            let _ = resp_tx.send(round_response(
                &session.id().await,
                RoundEvent::Notice(notice),
            ));
        }
        let _ = resp_tx.send(round_response(
            &session.id().await,
            RoundEvent::DelegatedChanged(restored_delegated),
        ));
    }

    let mut messages = session.model_window().await;
    agent.fire_session_start(source, &mut messages).await;
    if let Err(err) = session.replace_messages(messages).await {
        tracing::warn!(error = %err, "failed to persist SessionStart hook context");
    }

    send_harness_state_for_session(
        resp_tx,
        &session.id().await,
        agent,
        session,
        LoopStatus::Idle,
    )
    .await;
}

pub(crate) async fn fork_current_session(
    lifecycle: &Arc<RoundLifecycle>,
    agent: &Agent,
    session: &Arc<SessionStore>,
    side: &Arc<AsyncRwLock<SideRegistry>>,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    name: &str,
    args: &str,
) {
    supersede_for_session_switch(lifecycle, agent, resp_tx).await;
    teardown_sides_for_session_switch(side, resp_tx).await;
    match session.fork().await {
        Ok((id, parent_id)) => {
            agent.restore_round_count(session.round_counter().await);
            record_command(
                session,
                resp_tx,
                name,
                args,
                CommandResult::Text(format!("Forked session {} from {}.", id, parent_id)),
            )
            .await;
            send_harness_state_for_session(
                resp_tx,
                &session.id().await,
                agent,
                session,
                LoopStatus::Idle,
            )
            .await;
        }
        Err(error) => {
            record_error(session, resp_tx, name, args, error).await;
        }
    }
}
