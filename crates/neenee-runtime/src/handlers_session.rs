//! Session-management, tool-toggle, and `/btw` aside handlers, extracted
//! verbatim from the agent background task's `match req { … }` dispatch.
//!
//! Each handler is one match arm, lifted unchanged. Parameters are named to
//! match the original loop locals (`session`, `agent`, `resp_tx`, `side`, …)
//! so the body reads exactly as it did inline.

use neenee_agent::Agent;
use neenee_agent::orchestration::send_harness_state;
use neenee_contracts::{AgentResponse, LoopStatus, SessionOverview};
use neenee_mcp::McpRuntime;
use neenee_persistence::{config::Config, session::SessionStore};
use neenee_skills::SkillRegistry;
use std::sync::Arc;
use tokio::sync::{RwLock as AsyncRwLock, mpsc};

use crate::session_view::{build_session_context, build_sessions_overview};
use crate::side::{SideRegistry, SideSession};

/// `AgentRequest::DeleteSession` — delete by id (or short-id prefix) and push
/// a fresh sessions-overview snapshot, or surface the storage error.
pub async fn delete(
    session: &Arc<SessionStore>,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    id: String,
) {
    match session.delete(&id).await {
        Ok(()) => {
            let _ = resp_tx.send(AgentResponse::SessionsOverview(
                build_sessions_overview(session).await,
            ));
        }
        Err(error) => {
            let _ = resp_tx.send(AgentResponse::Error(error));
        }
    }
}

/// `AgentRequest::RenameSession` — set (or clear) a session's manual title by
/// id (or short-id prefix) and push a fresh sessions-overview snapshot, or
/// surface the storage error. `title = None` clears the manual override, so
/// the overview falls back to the AI-title / first-prompt preview (ADR-0022).
/// The pushed overview also refreshes the hosted session's monitor row: the
/// registry's broadcast-tap folds it into the tracker and republishes
/// `MonitorEvent::SessionUpdated` with the new title.
pub async fn rename(
    session: &Arc<SessionStore>,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    id: String,
    title: Option<String>,
) {
    match session.rename(&id, title).await {
        Ok(()) => {
            let mut overview = build_sessions_overview(session).await;
            // A rename on the live, never-persisted session (empty
            // transcript) is absent from the disk-backed list — but the
            // monitor re-seeds its row from this snapshot, so the new title
            // would be invisible until the first message lands. Synthesize
            // the row from in-memory state in that case.
            let matches_id =
                |row_id: &str| row_id == id || (id.len() >= 4 && row_id.starts_with(id.as_str()));
            if !overview.iter().any(|row| matches_id(&row.id)) {
                let summary = session.active_summary().await;
                if matches_id(&summary.id) {
                    overview.push(SessionOverview {
                        id: summary.id,
                        overview: summary.overview,
                        created_at: summary.created_at,
                        updated_at: summary.updated_at,
                        message_count: summary.message_count,
                        active: summary.active,
                        parent_id: summary.parent_id,
                        fork_kind: summary.fork_kind,
                    });
                }
            }
            let _ = resp_tx.send(AgentResponse::SessionsOverview(overview));
        }
        Err(error) => {
            let _ = resp_tx.send(AgentResponse::Error(error));
        }
    }
}

/// `AgentRequest::QuerySessionDetail` — full detail for one session (complete
/// last prompt, title, timestamps). Reply with [`AgentResponse::SessionDetail`]
/// for the session-info sub-view, or surface the storage error.
pub async fn detail(
    session: &Arc<SessionStore>,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    id: String,
) {
    match session.detail(&id).await {
        Ok(detail) => {
            let _ = resp_tx.send(AgentResponse::SessionDetail(detail));
        }
        Err(error) => {
            let _ = resp_tx.send(AgentResponse::Error(error));
        }
    }
}

/// `AgentRequest::QueryTokenUsage` — snapshot the server-side token-source
/// ledger for one session and reply with
/// [`AgentResponse::TokenUsageReport`]. Attached frontends hold no local
/// ledger, so the context-usage modal reads the daemon's accounting through
/// this on-demand round-trip. Pure read: the ledger is shared across sessions
/// and filtered by `session_id`, so an unknown/empty id simply yields an
/// empty report.
pub fn token_usage(
    token_ledger: &neenee_contracts::TokenSourceLedger,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    session_id: String,
) {
    let report = token_ledger.snapshot_for_session(&session_id);
    let _ = resp_tx.send(AgentResponse::TokenUsageReport { session_id, report });
}

/// `AgentRequest::QuerySessionContext` — build and push the
/// model/tools/permissions/skills/mcp snapshot for the Tools / Mcp / Skills /
/// Permissions manager modals.
pub fn query_context(
    agent: &Agent,
    skills_registry: &Arc<SkillRegistry>,
    mcp_runtime: &Arc<McpRuntime>,
    config: &Config,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
) {
    let snapshot = build_session_context(
        agent,
        skills_registry,
        &mcp_runtime.statuses_snapshot(),
        config,
    );
    let _ = resp_tx.send(AgentResponse::SessionContext(snapshot));
}

/// `AgentRequest::RevokePermission` — drop one cached always-allow rule and
/// push a refreshed snapshot, or report there was nothing matching.
pub fn revoke_permission(
    agent: &Agent,
    skills_registry: &Arc<SkillRegistry>,
    mcp_runtime: &Arc<McpRuntime>,
    config: &Config,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    tool: String,
    scope: String,
) {
    let removed = agent.revoke_allowed_tool(&tool, &scope);
    if removed {
        let snapshot = build_session_context(
            agent,
            skills_registry,
            &mcp_runtime.statuses_snapshot(),
            config,
        );
        let _ = resp_tx.send(AgentResponse::SessionContext(snapshot));
    } else {
        let _ = resp_tx.send(AgentResponse::Error(format!(
            "No cached always-allow rule for {} {}.",
            tool, scope
        )));
    }
}

/// `AgentRequest::ClearAllPermissions` — drop every cached always-allow rule
/// for this process and push a refreshed snapshot so the permissions manager
/// modal reflects the now-empty list.
pub fn clear_all_permissions(
    agent: &Agent,
    skills_registry: &Arc<SkillRegistry>,
    mcp_runtime: &Arc<McpRuntime>,
    config: &Config,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
) {
    agent.clear_allowed_tools();
    let snapshot = build_session_context(
        agent,
        skills_registry,
        &mcp_runtime.statuses_snapshot(),
        config,
    );
    let _ = resp_tx.send(AgentResponse::SessionContext(snapshot));
}

/// `AgentRequest::ToggleTool` — enable/disable a tool for the session and
/// push a refreshed snapshot. A no-op (unknown tool, or already in the target
/// state) still refreshes the snapshot so the modal settles rather than
/// leaving the row looking stale, plus surfaces a soft error.
pub fn toggle_tool(
    agent: &Agent,
    skills_registry: &Arc<SkillRegistry>,
    mcp_runtime: &Arc<McpRuntime>,
    config: &Config,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    name: String,
    enabled: bool,
) {
    let changed = agent.set_tool_enabled(&name, enabled);
    let snapshot = build_session_context(
        agent,
        skills_registry,
        &mcp_runtime.statuses_snapshot(),
        config,
    );
    if !changed {
        let _ = resp_tx.send(AgentResponse::Error(format!(
            "Tool '{}' is unknown or already {}.",
            name,
            if enabled { "enabled" } else { "disabled" }
        )));
    }
    let _ = resp_tx.send(AgentResponse::SessionContext(snapshot));
}

/// `AgentRequest::ToggleMcpServer` — connect/disconnect a configured MCP server
/// for the live session (session-scoped; config.toml is untouched). The runtime
/// rebuilds the agent's tool list, then we push a refreshed snapshot. A failure
/// to connect surfaces as a soft error but still refreshes the snapshot so the
/// row settles on its new (Failed) status.
pub async fn toggle_mcp_server(
    agent: &Agent,
    skills_registry: &Arc<SkillRegistry>,
    mcp_runtime: &Arc<McpRuntime>,
    config: &Config,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    name: String,
    enabled: bool,
) {
    if let Err(error) = mcp_runtime.set_enabled(&name, enabled).await {
        let _ = resp_tx.send(AgentResponse::Error(error));
    }
    let snapshot = build_session_context(
        agent,
        skills_registry,
        &mcp_runtime.statuses_snapshot(),
        config,
    );
    let _ = resp_tx.send(AgentResponse::SessionContext(snapshot));
}

/// `AgentRequest::ReconnectMcpServer` — re-establish one server's connection on
/// demand (the `/mcp` modal's `r` action) and push a refreshed snapshot.
pub async fn reconnect_mcp_server(
    agent: &Agent,
    skills_registry: &Arc<SkillRegistry>,
    mcp_runtime: &Arc<McpRuntime>,
    config: &Config,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    name: String,
) {
    if let Err(error) = mcp_runtime.reconnect(&name).await {
        let _ = resp_tx.send(AgentResponse::Error(error));
    }
    let snapshot = build_session_context(
        agent,
        skills_registry,
        &mcp_runtime.statuses_snapshot(),
        config,
    );
    let _ = resp_tx.send(AgentResponse::SessionContext(snapshot));
}

/// `AgentRequest::ExitSideView` — detach from the `/btw` aside view and return
/// to the primary transcript (ADR-0103 §1). Non-destructive by default: the
/// aside's in-flight round is left alone and its session stays registered so
/// it can be re-entered later (the asides list shows it). The one carve-out
/// is the pristine rule (§4): an aside that never started a round has no user
/// content of its own, so it is dropped from the registry **and** its session
/// files are deleted — an opened-then-abandoned `/btw` never litters.
pub async fn detach_side_view(
    side: &Arc<AsyncRwLock<SideRegistry>>,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
) {
    let detached_id = side.write().await.detach();
    if let Some(id) = detached_id.as_deref() {
        let pristine = side
            .read()
            .await
            .get(id)
            .is_some_and(SideSession::is_pristine);
        if pristine {
            // Discard: cancel nothing (there is no round by definition),
            // drop the registry entry, delete the forked files.
            if let Some(s) = side.write().await.remove(id) {
                s.agent.reject_pending_permissions();
                let _ = s.store.delete(&s.id).await;
            }
            crate::side::publish_btw_list(side, resp_tx).await;
        }
    }
    let _ = resp_tx.send(AgentResponse::SideViewClosed);
}

/// `AgentRequest::FocusSide` — jump the view into a live aside (ADR-0103 §5).
/// Re-opens it if needed, makes it the composer target, and emits
/// `SideViewOpened` carrying the aside's full persisted transcript so the
/// frontend rebuilds its side buffer (inherited parent context included —
/// ADR-0103 §6).
pub async fn focus_side(
    side: &Arc<AsyncRwLock<SideRegistry>>,
    primary_session: &Arc<SessionStore>,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    side_id: String,
) {
    let focused = side.write().await.focus(&side_id);
    if !focused {
        let _ = resp_tx.send(AgentResponse::Error(
            "That aside is no longer open.".to_string(),
        ));
        return;
    }
    emit_side_view_opened(side, primary_session, resp_tx, &side_id).await;
}

/// Build and send the `SideViewOpened` event for a registered aside: routing
/// keys plus the one-shot transcript back-fill (§6). Shared by `/btw` (new
/// aside) and `focus_side` (re-entry).
pub async fn emit_side_view_opened(
    side: &Arc<AsyncRwLock<SideRegistry>>,
    primary_session: &Arc<SessionStore>,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    side_id: &str,
) {
    let handle = side.read().await.handle(side_id);
    let Some(s) = handle else {
        return;
    };
    let messages = s.store.full_transcript().await;
    let commands = s.store.commands().await;
    let primary_id = primary_session.id().await;
    let _ = resp_tx.send(AgentResponse::SideViewOpened {
        side_id: s.id.clone(),
        primary_id,
        messages,
        commands,
    });
}

/// `AgentRequest::InterruptSide` — interrupt the in-flight round of one aside
/// (ADR-0103 §2). Esc inside an aside view resolves here; interrupting an
/// aside never closes it. Mirrors the primary interrupt's eager idle flip so
/// the aside's own activity surfaces collapse immediately, without touching
/// the primary's lifecycle.
pub async fn interrupt_side(
    side: &Arc<AsyncRwLock<SideRegistry>>,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    side_id: String,
) {
    let target = side.read().await.handle(&side_id);
    let Some(s) = target else {
        return;
    };
    s.agent.reject_pending_permissions();
    s.agent.reject_pending_user_questions();
    s.agent.reject_pending_inputs();
    // Session-scoped PermissionsCleared + eager idle snapshot, exactly like
    // the primary's `interrupt` — but scoped to the aside's session id so the
    // primary chrome is untouched.
    let _ = resp_tx.send(AgentResponse::PermissionsCleared);
    send_harness_state(resp_tx, &s.id, &s.agent, LoopStatus::Idle);
    s.lifecycle.cancel_current().await;
}

/// `AgentRequest::CloseSide` — close one aside for real (ADR-0103 §5, the
/// asides modal's `D` action): cancel any in-flight round, drop the registry
/// entry, and delete the aside's session files so it disappears from the
/// asides list and `/sessions`. If the closed aside was the focused view, the
/// harness also emits `SideViewClosed`.
pub async fn close_side(
    side: &Arc<AsyncRwLock<SideRegistry>>,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    side_id: String,
) {
    let was_active = side.read().await.active().is_some_and(|s| s.id == side_id);
    if let Some(s) = side.write().await.remove(&side_id) {
        s.agent.reject_pending_permissions();
        s.agent.reject_pending_user_questions();
        s.agent.reject_pending_inputs();
        s.lifecycle.cancel_current().await;
        let _ = s.store.delete(&s.id).await;
    }
    crate::side::publish_btw_list(side, resp_tx).await;
    if was_active {
        let _ = resp_tx.send(AgentResponse::SideViewClosed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use neenee_contracts::Message;

    /// A store with one persisted user prompt (so it appears in `list()`),
    /// plus the response channel a frontend would hold.
    async fn store_with_prompt() -> (tempfile::TempDir, Arc<SessionStore>) {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(SessionStore::for_path(dir.path().join("session.json")));
        store
            .replace_messages(vec![Message::new(
                neenee_contracts::Role::User,
                "first prompt",
            )])
            .await
            .unwrap();
        (dir, store)
    }

    #[tokio::test]
    async fn rename_replies_with_a_fresh_sessions_overview() {
        let (_dir, store) = store_with_prompt().await;
        let id = store.id().await;
        let (resp_tx, mut resp_rx) = mpsc::unbounded_channel();

        rename(
            &store,
            &resp_tx,
            id[..8].to_string(),
            Some("new title".to_string()),
        )
        .await;

        let Some(AgentResponse::SessionsOverview(items)) = resp_rx.recv().await else {
            panic!("expected a sessions-overview push after a rename");
        };
        let row = items.iter().find(|item| item.id == id).unwrap();
        assert_eq!(row.overview, "new title");
        let (title, manual) = store.title().await;
        assert_eq!(title.as_deref(), Some("new title"));
        assert!(manual);
    }

    #[tokio::test]
    async fn rename_unknown_id_replies_with_the_storage_error() {
        let (_dir, store) = store_with_prompt().await;
        let (resp_tx, mut resp_rx) = mpsc::unbounded_channel();

        rename(&store, &resp_tx, "deadbeef".to_string(), None).await;

        let Some(AgentResponse::Error(error)) = resp_rx.recv().await else {
            panic!("expected an error reply for an unknown id");
        };
        assert_eq!(error, "No session matches 'deadbeef'.");
    }
}
