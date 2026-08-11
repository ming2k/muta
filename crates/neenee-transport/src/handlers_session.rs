//! Session-management, tool-toggle, and `/btw` side-view handlers, extracted
//! verbatim from the agent background task's `match req { … }` dispatch.
//!
//! Each handler is one match arm, lifted unchanged. Parameters are named to
//! match the original loop locals (`session`, `agent`, `resp_tx`, `side`,
//! `active_view_side`, …) so the body reads exactly as it did inline.

use neenee_agent::Agent;
use neenee_agent::mcp::McpRuntime;
use neenee_core::AgentResponse;
use neenee_persistence::{config::Config, session::SessionStore};
use neenee_skills::SkillRegistry;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::{RwLock as AsyncRwLock, mpsc};

use crate::session_view::{build_session_context, build_sessions_overview};
use crate::side::SideSession;

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
    token_ledger: &neenee_core::TokenSourceLedger,
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

/// `AgentRequest::ExitSideView` — tear down the live `/btw` side session
/// (ADR-0017). Any in-flight side round is cancelled; the side file stays on
/// disk, recoverable via `/sessions`. The primary round — if running — is
/// untouched.
pub async fn exit_side_view(
    side: &Arc<AsyncRwLock<Option<SideSession>>>,
    active_view_side: &AtomicBool,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
) {
    if let Some(s) = side.write().await.take() {
        s.agent.reject_pending_permissions();
        s.lifecycle.cancel_current().await;
    }
    active_view_side.store(false, Ordering::SeqCst);
    let _ = resp_tx.send(AgentResponse::SideViewClosed);
}
