//! The `AgentRequest::SlashCommand` dispatcher, extracted verbatim from the
//! agent background task's `match req { … }` dispatch.
//!
//! This is the largest handler — it fans the parsed command out across every
//! `BuiltinCmd` variant (`/models`, `/mcp`, `/compact`, `/new`,
//! `/permissions`, `/delegate`, `/review`, `/search`, `/resume`,
//! `/session`, `/sessions`, `/btw`, `/repeat`, `/schedule`, `/init`,
//! `/skills`, `/skill`, `/export`, `/debug`, `/help`, `/exit`) plus the
//! `None` arm that runs a user-defined project command.
//!
//! The body is the original inline match arm, lifted unchanged except that
//! every loop-level `continue` is now a function-level `return` (semantically
//! identical: the caller's `while let` proceeds to the next request either
//! way). Parameters are named to match the original loop locals so the body
//! reads exactly as it did inline.
//!
//! NOTE: a `refresh_agent_pursuit` + SessionStart-hooks block inside the
//! `/pursue status` branch has inconsistent indentation and looks misplaced —
//! it fires session-start hooks every time `/pursue status` runs. Preserved
//! verbatim; not this refactor's job to fix.

use crate::commands::{CustomCommand, expand_command};
use crate::project::init_muta_config;
use muta_agent::Agent;
use muta_agent::RoundLifecycle;
use muta_agent::orchestration::{
    ContextProjectionSettings, RoundInput, compact_round_history, round_response, send_compaction,
    send_harness_state,
};
use muta_contracts::{
    AgentNotice, AgentRequest, AgentResponse, CommandRecord, CommandResult, CronExpr, LoopStatus,
    Message, Provider, RoundEvent, Schedule, ScheduledJob, Tool, TrustDomain, estimate_tokens,
    repeat::parse_schedule_arg,
};
use muta_mcp::McpRuntime;
use muta_persistence::{
    config::Config, connection_usage::ConnectionUsage, embedding, session::SessionStore,
    workspace_security::WorkspaceSecurityStore,
};
use muta_skills::{ListSkillsTool, SkillRegistry, UseSkillTool};

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use tokio::sync::{RwLock as AsyncRwLock, mpsc};

fn runtime_workspace_security(
    store: &WorkspaceSecurityStore,
    root: &std::path::Path,
) -> muta_contracts::WorkspaceSecuritySnapshot {
    store.snapshot(root)
}

fn live_custom_commands(
    store: &WorkspaceSecurityStore,
    root: &std::path::Path,
) -> HashMap<String, CustomCommand> {
    let rules_state = runtime_workspace_security(store, root).rules;
    crate::commands::discover_commands_with_trust(root, rules_state)
        .commands
        .into_iter()
        .map(|command| (command.name.clone(), command))
        .collect()
}

struct AssetReloadReport {
    snapshot: muta_contracts::WorkspaceSecuritySnapshot,
    connected_mcp: Vec<String>,
    removed_mcp: Vec<String>,
}

/// Rebuild every project-asset consumer from one freshly attested snapshot.
/// This is the only live apply/unload path for `/trust` and `/untrust`.
async fn reload_trusted_assets(
    agent: &Arc<Agent>,
    mcp_runtime: &Arc<McpRuntime>,
    workspace_security: &WorkspaceSecurityStore,
    project_root: &std::path::Path,
    skills_registry: &SkillRegistry,
) -> Result<AssetReloadReport, String> {
    let snapshot = workspace_security.snapshot(project_root);
    let mut effective = Config::load();
    if snapshot.mcp.is_trusted() {
        effective.merge_project_mcp(Config::load_project_mcp(project_root));
    }
    if snapshot.hooks.is_trusted() {
        effective.merge_project_hooks(Config::load_project_hooks(project_root));
    }
    if snapshot.roots.is_trusted() {
        effective
            .merge_project_additional_roots(Config::load_project_additional_roots(project_root));
    }

    let mcp_report = mcp_runtime.reconfigure(effective.mcp.clone()).await;
    agent.set_hooks(crate::hooks::build_hook_registry(&effective.hooks, agent));
    skills_registry.reload().await;
    let rules = if snapshot.rules.is_trusted() {
        crate::project::load_project_rules(project_root)?
    } else {
        String::new()
    };
    agent.set_project_rules(rules);
    agent.set_workspace_security(snapshot.clone());

    Ok(AssetReloadReport {
        snapshot,
        connected_mcp: mcp_report
            .connected
            .into_iter()
            .filter_map(|(name, ok)| ok.then_some(name))
            .collect(),
        removed_mcp: mcp_report.removed,
    })
}

use crate::agent_setup::active_context_window;
use crate::session_view::{build_sessions_overview, short_session_id};
use crate::side::SideEnv;
use crate::side::{
    SideRegistry, SideSession, publish_btw_list, refuse_if_no_provider,
    spawn_parent_status_watcher, start_active_turn,
};
use crate::slash_handler::{SlashCommandRegistry, SlashContext};
use crate::startup::{BuiltinCmd, SessionStart, split_custom_command};

async fn supersede_for_session_switch(
    lifecycle: &RoundLifecycle,
    agent: &Agent,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
) {
    // Park the reason before superseding so the in-flight round's suppressed
    // tail still records *why* it died (C11): a session switch supersedes it.
    lifecycle.record_interrupt(muta_contracts::RoundInterruptReason::Superseded);
    lifecycle.supersede();
    agent.reject_pending_permissions();
    agent.reject_pending_user_questions();
    agent.reject_pending_inputs();
    let _ = resp_tx.send(AgentResponse::PermissionsCleared);
    lifecycle.cancel_current().await;
}

/// Recompute the admitted additional-root set from the current trust state
/// and swap it into the live handle (ADR-0147). This is the runtime half of
/// "the workspace boundary takes shape at trust-decision time": a grant
/// widens admission on the next confined operation; a revoke or reloaded
/// config collapses it back to the primary. Replace semantics — never union
/// with the previous set — keep fail-closed on every path.
///
/// Global roots are user-owned and always admitted; project roots only when
/// the snapshot's `roots` domain is trusted. Resolution failures (missing or
/// nested directories) drop that root and keep the rest, matching the
/// startup loader's per-entry tolerance for live edits.
pub(crate) fn apply_additional_roots(
    handle: &muta_contracts::SharedAdditionalRoots,
    effective: &Config,
    project_root: &std::path::Path,
    roots_state: muta_contracts::WorkspaceTrustState,
) {
    let merged = if roots_state.is_trusted() {
        let mut all = effective.workspace.additional_roots.clone();
        for root in Config::load_project_additional_roots(project_root) {
            if !all.contains(&root) {
                all.push(root);
            }
        }
        all
    } else {
        // User-owned global entries stay; project declarations fall out.
        effective.workspace.additional_roots.clone()
    };
    let resolved = resolve_roots_from_strings(&merged, project_root);
    handle.store(resolved);
}

/// Resolve raw root strings to the canonical existing-directory set, mirroring
/// the startup filter: expand `~`, make absolute against the project root,
/// canonicalize, require a directory outside the primary, dedupe.
fn resolve_roots_from_strings(raws: &[String], project_root: &std::path::Path) -> Vec<PathBuf> {
    let canonical_root =
        std::fs::canonicalize(project_root).unwrap_or_else(|_| project_root.to_path_buf());
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    let mut resolved = Vec::new();
    for raw in raws {
        let expanded = match raw.as_str() {
            "~" => home.clone(),
            r => {
                if let Some(rest) = r.strip_prefix("~/") {
                    home.as_ref().map(|h| h.join(rest))
                } else {
                    let p = std::path::PathBuf::from(r);
                    Some(if p.is_absolute() { p } else { canonical_root.join(p) })
                }
            }
        };
        let Some(path) = expanded else { continue };
        let Ok(canonical) = std::fs::canonicalize(&path) else {
            continue;
        };
        if !canonical.is_dir() {
            continue;
        }
        if canonical == canonical_root || canonical.starts_with(&canonical_root) {
            continue;
        }
        if !resolved.contains(&canonical) {
            resolved.push(canonical);
        }
    }
    resolved
}

/// Session-switch companion to [`supersede_for_session_switch`]: tear down
/// every live `/btw` aside before the driver repoints at another session
/// (ADR-0103). Asides are forks of the *outgoing* session — carrying them
/// across a switch would leave them composing into the wrong parent's store
/// routing — so each is cancelled (its own lifecycle), dropped from the
/// registry, and deleted from disk, mirroring the pristine-discard rule: a
/// switch is an explicit context change, not a "keep it running" leave.
/// Their files are removed because an aside that is never re-enterable is
/// exactly the abandoned-`/btw` litter the discard rule exists to prevent.
pub(crate) async fn teardown_sides_for_session_switch(
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
            // The aside's files are deleted right below, so its round-interrupt
            // record is moot — but parking the reason still labels the unwind
            // if the aside's tail races the removal (C11).
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

/// Start a brand-new empty session and switch the live session to it — the
/// shared body of `/new` and the legacy `/session new`.
///
/// The outgoing session's file is left untouched on disk (nothing is wiped;
/// it stays resumable via `/sessions` or `/sessions <id>`). `SessionStore::reset`
/// mints a fresh id and defers persistence so an unused fresh session leaves
/// no empty file behind. Any in-flight round and pending prompts are
/// superseded, the live round counter is rebased to the fresh store, and the
/// provider falls back to the global default (a fresh session carries no
/// provider pin, C6). The frontend is told through `ConversationCleared`
/// (blank transcript, zeroed round count) plus a `TodosUpdated` reset, and
/// the confirmation lands in the new session's command ledger.
async fn start_fresh_session(env: &mut SlashEnv<'_>, name: &str, args: &str) {
    // Split-borrow: the fresh-session path mutates usage while reading the
    // rest of the environment.
    let (side, session, config, agent, lifecycle, resp_tx, provider_for_task) = (
        env.side,
        env.session,
        env.config,
        env.agent,
        env.lifecycle,
        env.resp_tx,
        env.provider_for_task,
    );
    let provider_usage = &mut *env.provider_usage;
    supersede_for_session_switch(lifecycle, agent, resp_tx).await;
    teardown_sides_for_session_switch(side, resp_tx).await;
    agent.clear_todos();
    match session.reset().await {
        Ok(id) => {
            // ADR-0132: `/reset` starts a *new* session. The persisted
            // delegated posture belongs to the old one — a fresh session
            // must not inherit an unattended posture the user has not
            // (re-)granted to it. The store's `reset` already minted fresh
            // data (delegated = false); re-align the live agent so the old
            // session's posture does not leak across the boundary, and
            // broadcast the (possibly de-escalated) posture so the TUI
            // badge stops lying about the new session.
            let fresh_posture = session.delegated().await;
            if agent.delegated() != fresh_posture {
                agent.set_delegated(fresh_posture);
                let _ = resp_tx.send(round_response(&id, RoundEvent::DelegatedChanged(fresh_posture)));
            }
            agent.restore_round_count(session.round_counter().await);
            // C6: a fresh session has no provider pin, so the live provider
            // falls back to the global default.
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

/// Full session-scoped runtime restore, run after the session store has been
/// repointed at a different session (via `/session open`, `/resume`, …).
///
/// This mirrors the restore block the bootstrap skips in Picker mode
/// (`mutx attach` with no id): the unified task list, the disabled-tool
/// mask, the round counter, and the SessionStart hooks. `fire_session_start`
/// lives only in `bootstrap` otherwise, so a session chosen from the startup
/// picker would otherwise never receive its hook-injected setup context.
///
/// `source` is surfaced to SessionStart hooks (`Startup` vs `Resume`) so a
/// hook can branch — opening a prior session from the picker is a resume.
///
/// Fork the current session into a child branch — the shared body of `/fork`
/// and the legacy `/session fork`. The parent's file is untouched; the store
/// repoints at the child, the live round counter follows, and the loop
/// checkpoint is superseded so the child starts its own rounds.
async fn fork_current_session(
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
            send_harness_state(resp_tx, &session.id().await, agent, LoopStatus::Idle);
        }
        Err(error) => {
            record_error(session, resp_tx, name, args, error).await;
        }
    }
}

/// Emits the restored todos as a `TodosUpdated` event so the frontend's sticky
/// panel appears the moment the user enters the picked session.
async fn restore_session_runtime(
    session: &Arc<SessionStore>,
    agent: &Arc<Agent>,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    source: muta_contracts::SessionSource,
) {
    // Restore the task list. An empty list is the "no active task list" state
    // and is still emitted so the frontend clears a stale panel.
    let todos = session.todos().await;
    agent.set_todos(todos.clone());
    let _ = resp_tx.send(round_response(
        &session.id().await,
        RoundEvent::TodosUpdated(todos),
    ));

    // The orthogonal tool mask and round counter.
    agent.restore_disabled_tools(session.disabled_tools().await);
    agent.restore_round_count(session.round_counter().await);

    // Restore the session-scoped delegated posture (ADR-0132). The flag is
    // now persisted on the session (`AutopilotSet`), so a resumed session —
    // whether via `/sessions <id>` in-process, a fresh attach after a daemon
    // crash, or a boot rehost — reopens in the posture it left. The restore
    // is an alignment, not a one-way escalation: switching from an
    // unattended session to an attended one must de-escalate too, or the
    // attended session would silently run with the previous session's
    // blanket permissions.
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

    // SessionStart hooks (ADR-0025): inject setup context before the first
    // round of the freshly entered session. Persist through the single write
    // path so the session stays the source of truth (ADR-0048).
    let mut messages = session.model_window().await;
    agent.fire_session_start(source, &mut messages).await;
    if let Err(err) = session.replace_messages(messages).await {
        tracing::warn!(error = %err, "failed to persist SessionStart hook context");
    }
}

// ── Command-ledger helpers (ADR-0091) ──────────────────────────────────────
//
// Slash commands are operations on the session, not conversation turns: each
// invocation + its typed result are recorded in the durable command ledger and
// surfaced as a `RoundEvent::CommandResult` command block — never as
// `RoundEvent::Text` assistant prose. These helpers are the single seam;
// `record_command` is best-effort (a failed persist logs but does not abort the
// command's primary effect).

/// Record a successful slash-command invocation in the ledger and surface its
/// typed result as a command block (the ADR-0091 replacement for a
/// `RoundEvent::Text` command reply).
async fn record_command(
    session: &Arc<SessionStore>,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    name: &str,
    args: &str,
    result: CommandResult,
) {
    record_command_with_duration(session, resp_tx, name, args, result, None).await;
}

async fn record_command_with_duration(
    session: &Arc<SessionStore>,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    name: &str,
    args: &str,
    result: CommandResult,
    duration_ms: Option<u64>,
) {
    let mut record = CommandRecord::new(name, args).with_result(result.clone());
    if let Some(ms) = duration_ms {
        record = record.with_duration_ms(ms);
    }
    if let Err(error) = session.mutate_commands(|c| c.push(record)).await {
        tracing::warn!(?error, name, "could not persist command record");
    }
    let _ = resp_tx.send(round_response(
        &session.id().await,
        RoundEvent::CommandResult {
            name: name.to_string(),
            args: args.to_string(),
            result,
        },
    ));
}

/// Record a slash-command invocation whose reply is a special response type
/// (a picker like `/sessions`, a side view like `/btw`, a compaction
/// checkpoint, `ConversationCleared`/`Replaced`, exit) rather than a command
/// block. The invocation is durable; there is no `CommandResult` to display.
async fn record_invocation(session: &Arc<SessionStore>, name: &str, args: &str) {
    record_invocation_with_duration(session, name, args, None).await;
}

async fn record_invocation_with_duration(
    session: &Arc<SessionStore>,
    name: &str,
    args: &str,
    duration_ms: Option<u64>,
) {
    let mut record = CommandRecord::new(name, args);
    if let Some(ms) = duration_ms {
        record = record.with_duration_ms(ms);
    }
    if let Err(error) = session.mutate_commands(|c| c.push(record)).await {
        tracing::warn!(?error, name, "could not persist command invocation");
    }
}

/// Record an acknowledgment — the durable twin of an ADR-0088 `CommandAck`
/// toast. The live surface stays the toast; the ledger keeps the confirmation
/// for resume/export/audit. No `CommandResult` event is emitted, so a command
/// block never double-renders the toast.
async fn record_ack(session: &Arc<SessionStore>, name: &str, args: &str, title: impl Into<String>) {
    record_ack_with_duration(session, name, args, title, None).await;
}

async fn record_ack_with_duration(
    session: &Arc<SessionStore>,
    name: &str,
    args: &str,
    title: impl Into<String>,
    duration_ms: Option<u64>,
) {
    let mut record = CommandRecord::new(name, args).with_result(CommandResult::Ack {
        title: title.into(),
        detail: None,
    });
    if let Some(ms) = duration_ms {
        record = record.with_duration_ms(ms);
    }
    if let Err(error) = session.mutate_commands(|c| c.push(record)).await {
        tracing::warn!(?error, name, "could not persist command ack");
    }
}

/// Record a failed slash-command invocation and surface the error as a
/// typed `CommandResult::Error` (ADR-0091, ADR-0108). The command component in
/// the transcript settles in place with the error message rather than emitting
/// a separate notification.
async fn record_error(
    session: &Arc<SessionStore>,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    name: &str,
    args: &str,
    message: impl Into<String>,
) {
    record_error_with_duration(session, resp_tx, name, args, message, None).await;
}

async fn record_error_with_duration(
    session: &Arc<SessionStore>,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    name: &str,
    args: &str,
    message: impl Into<String>,
    duration_ms: Option<u64>,
) {
    let message = message.into();
    record_command_with_duration(
        session,
        resp_tx,
        name,
        args,
        CommandResult::Error {
            message,
            detail: None,
        },
        duration_ms,
    )
    .await;
}

/// Where a `/sessions`-family invocation routes. `/sessions` and the retired
/// `/resume` / `/session` spellings all resolve through
/// [`BuiltinCmd::Sessions`]; this enum is the pure grammar decision, so the
/// mapping is unit-testable apart from the dispatch plumbing.
#[derive(Debug, PartialEq, Eq)]
enum SessionRoute<'a> {
    /// Open a session by id (or `None` for the picker).
    Open(Option<&'a str>),
    /// Start a fresh session (`/new` semantics; legacy `/session new`).
    New,
    /// Fork the current session (legacy `/session fork`).
    Fork,
    /// Legacy `/session status` — retired with guidance, no action.
    Status,
}

/// Decide [`SessionRoute`] for a `/sessions`-family command. `name` is the
/// command word without the slash (`sessions`, `resume`, or `session`);
/// `parts` is the whitespace-split invocation with `parts[0]` the command.
/// `Err` carries the unknown-legacy-subcommand message.
fn session_route<'a>(name: &str, parts: &'a [&str]) -> Result<SessionRoute<'a>, String> {
    if name != "session" {
        // Canonical `/sessions <id?>` (and legacy `/resume <id?>`, whose id
        // sits in the same slot).
        return Ok(SessionRoute::Open(parts.get(1).copied()));
    }
    match parts.get(1).copied().unwrap_or("") {
        // The id moved one slot right in the legacy spelling.
        "open" | "resume" => Ok(SessionRoute::Open(parts.get(2).copied())),
        "" => Ok(SessionRoute::Open(None)),
        "list" => Ok(SessionRoute::Open(None)),
        "new" => Ok(SessionRoute::New),
        "fork" => Ok(SessionRoute::Fork),
        "status" => Ok(SessionRoute::Status),
        unknown => Err(format!(
            "Unknown session command '{unknown}'. /session is retired: use /sessions to browse \
             or open, /new, or /fork."
        )),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TrustRoute {
    GrantAll,
    Grant(TrustDomain),
    Revoke,
    Status,
}

fn trust_route(name: &str, parts: &[&str]) -> Result<TrustRoute, String> {
    if name == "untrust" {
        return if parts.len() == 1 {
            Ok(TrustRoute::Revoke)
        } else {
            Err("/untrust accepts no arguments.".to_string())
        };
    }
    match parts.get(1).copied() {
        None | Some("all") => Ok(TrustRoute::GrantAll),
        Some("mcp") => Ok(TrustRoute::Grant(TrustDomain::Mcp)),
        Some("skills") => Ok(TrustRoute::Grant(TrustDomain::Skills)),
        Some("hooks") => Ok(TrustRoute::Grant(TrustDomain::Hooks)),
        Some("rules") => Ok(TrustRoute::Grant(TrustDomain::Rules)),
        Some("roots") => Ok(TrustRoute::Grant(TrustDomain::Roots)),
        Some("status") => Ok(TrustRoute::Status),
        Some("revoke") => Ok(TrustRoute::Revoke),
        Some(other) => Err(format!(
            "Unknown /trust subcommand '{other}'. Use `/trust`, `/trust all`, `/trust mcp`, \
             `/trust skills`, `/trust hooks`, `/trust rules`, `/trust roots`, `/trust status`, or `/trust revoke`."
        )),
    }
}

/// Bundled slash-dispatch environment: the daemon plumbing a slash command
/// needs beyond the command text itself. Extracted so `dispatch` reads as
/// `dispatch(cmd, env)` instead of a 22-parameter list threaded from the
/// single call site in `session_driver`.
pub(crate) struct SlashEnv<'a> {
    pub config: &'a Config,
    pub agent: &'a Arc<Agent>,
    pub mcp_runtime: &'a Arc<McpRuntime>,
    pub workspace_security: &'a Arc<WorkspaceSecurityStore>,
    /// Live additional-roots handle: a `/trust` grant or revoke recomputes
    /// the admitted set through it, effective on the next tool call.
    pub shared_additional_roots: &'a muta_contracts::SharedAdditionalRoots,
    pub resp_tx: &'a mpsc::UnboundedSender<AgentResponse>,
    pub session: &'a Arc<SessionStore>,
    pub lifecycle: &'a Arc<RoundLifecycle>,
    pub side: &'a Arc<AsyncRwLock<SideRegistry>>,
    pub base_tools_for_side: &'a Arc<Vec<Arc<dyn Tool>>>,
    pub provider_for_task: &'a Arc<RwLock<Arc<dyn Provider>>>,
    pub provider_usage: &'a mut ConnectionUsage,
    pub skills_registry: Arc<SkillRegistry>,
    pub skills_registry_for_commands: &'a Arc<SkillRegistry>,
    pub _commands_for_task: &'a HashMap<String, CustomCommand>,
    pub embedding_store_for_commands: &'a Arc<AsyncRwLock<embedding::EmbeddingStore>>,
    pub req_tx_for_commands: &'a mpsc::UnboundedSender<AgentRequest>,
    pub project_root_for_side: &'a std::path::Path,
    pub startup: &'a SessionStart,
    pub ui: &'a dyn crate::UiBridge,
    pub extra_commands: &'a SlashCommandRegistry,
    pub websearch_shared: &'a Arc<muta_contracts::SharedWebSearchConfig>,
}

/// `AgentRequest::SlashCommand` — parse the command, dispatch to the matching
/// built-in handler, or fall through to the user-defined project-command path.
pub(crate) async fn dispatch(cmd: String, mut env: SlashEnv<'_>) {
    let SlashEnv {
        config,
        agent,
        mcp_runtime,
        workspace_security,
        shared_additional_roots,
        resp_tx,
        session,
        lifecycle,
        side,
        base_tools_for_side,
        provider_for_task,
        ref mut provider_usage,
        ref skills_registry,
        skills_registry_for_commands,
        _commands_for_task,
        embedding_store_for_commands,
        req_tx_for_commands,
        project_root_for_side,
        startup,
        ui,
        extra_commands,
        websearch_shared,
    } = env;
    let parts: Vec<&str> = cmd.split_whitespace().collect();
    if parts.is_empty() {
        return;
    }
    let start_instant = std::time::Instant::now();
    // The ledger identity of this invocation (ADR-0091): command word without
    // the leading slash, plus the raw argument remainder.
    //
    // Alias normalization happens exactly once, HERE, and only in the ledger
    // identity — `cmd` keeps whatever spelling the user submitted. Completion
    // never rewrites an alias mid-typing (aliases are first-class candidates),
    // so dispatch is the single point where `/config` becomes `/settings`.
    let name = parts[0].trim_start_matches('/');
    let args = cmd.strip_prefix(parts[0]).unwrap_or("").trim();
    match BuiltinCmd::from_slash(parts[0]) {
        Some(BuiltinCmd::Models) | Some(BuiltinCmd::Connections) => {
            // Handled in TUI
        }
        Some(BuiltinCmd::Settings) => {
            // Bare `/settings` (and `/config`) is handled in the TUI: it opens the
            // settings manager modal locally for presentation settings (intercepted in
            // input.rs as `InputAction::OpenConfig`), so it never arrives
            // here. What does arrive is `/settings reload` (and the legacy
            // `/config reload` / `/reload` aliases): re-read config.toml and apply the diff live
            // (ADR-0085 §6) — MCP servers (diff + reconnect), project
            // MCP/hooks (trust-gated), bash policy, hooks registry,
            // permissions, master settings, tool variants, and the prune
            // threshold. User-triggered, no fs-watch.
            if parts.get(1) != Some(&"reload") {
                record_error(
                    session,
                    resp_tx,
                    name,
                    args,
                    "Unknown /settings command. Use /settings reload to re-read config.toml and \
                     apply it live.",
                )
                .await;
                return;
            }

            // ADR-0085 §6: re-read config.toml and apply the diff live, so
            // editing the MCP servers / permissions / bash policy / hooks no
            // longer requires a restart. Reload is user-triggered (not
            // fs-watch): only the user knows when their edit is complete, and
            // a half-written file would otherwise tear down live sessions.
            let mut reloaded = Config::load();
            // Re-apply project assets from one domain-specific trust snapshot.
            // No aggregate extension flag exists: each consumer checks only
            // the content it is about to load.
            let security_snapshot =
                runtime_workspace_security(workspace_security, project_root_for_side);
            let project_mcp = Config::load_project_mcp(project_root_for_side);
            if security_snapshot.mcp.is_trusted() && !project_mcp.is_empty() {
                reloaded.merge_project_mcp(project_mcp);
            }
            let project_hooks = Config::load_project_hooks(project_root_for_side);
            if security_snapshot.hooks.is_trusted() && !project_hooks.is_empty() {
                reloaded.merge_project_hooks(project_hooks);
            }
            if security_snapshot.roots.is_trusted() {
                let project_roots = Config::load_project_additional_roots(project_root_for_side);
                if !project_roots.is_empty() {
                    reloaded.merge_project_additional_roots(project_roots);
                }
            }
            // Live admitted-root swap (ADR-0147): the recomputed set replaces
            // — never unions — so an untrusted roots domain collapses
            // admission back to the primary immediately.
            apply_additional_roots(
                shared_additional_roots,
                &reloaded,
                project_root_for_side,
                security_snapshot.roots,
            );

            // MCP: diff + (re)connect/disconnect. The next request picks up the
            // new tool set automatically (visible_tools recomputes each turn).
            let report = mcp_runtime.reconfigure(reloaded.mcp.clone()).await;

            // Re-apply the agent-scoped config sections that are otherwise
            // seeded only at startup. Each setter is replace-style and safe to
            // re-run; permissions seeding is additive (new allow-rules take
            // effect; removed rules are noted but not revoked this session).
            agent.set_bash_policy(&reloaded.bash_policy);
            agent.set_hard_stop_turns(reloaded.master.hard_stop_turns);
            agent.set_doom_guard_config(reloaded.master.doom_guard);
            agent.set_allow_model_stdin(reloaded.master.allow_model_stdin);
            agent.set_skip_interactive_input(reloaded.master.skip_interactive_input);
            agent.set_hooks(crate::hooks::build_hook_registry(&reloaded.hooks, agent));
            skills_registry.reload().await;
            let project_rules = if security_snapshot.rules.is_trusted() {
                crate::project::load_project_rules(project_root_for_side).unwrap_or_else(|err| {
                    tracing::warn!(error = %err, "project rules reload failed; unloading rules");
                    String::new()
                })
            } else {
                String::new()
            };
            agent.set_project_rules(project_rules);
            agent.set_workspace_security(security_snapshot);
            agent.seed_permissions_from_config(&reloaded.permissions.allow);
            crate::agent_setup::reseed_prune_threshold(agent, &reloaded);
            crate::agent_setup::reseed_tool_variants(agent, &reloaded);
            // `[websearch]` is hot-reloadable too: push the re-read table
            // (already merged with credentials.toml by `Config::load`)
            // through the shared handle so the web tools pick up backend /
            // reader / proxy changes on their next call.
            crate::handlers_websearch::apply_reloaded(websearch_shared, &reloaded);

            let mut lines = Vec::new();
            lines.push(format!(
                "Web search: provider {}, fallback {}, reader {}.",
                reloaded.websearch.provider,
                if reloaded.websearch.fallback.trim().is_empty() {
                    "disabled"
                } else {
                    reloaded.websearch.fallback.trim()
                },
                reloaded.websearch.reader,
            ));
            if report.removed.is_empty()
                && report.connected.is_empty()
                && report.unchanged.is_empty()
            {
                lines.push("No MCP servers configured.".to_string());
            } else {
                if !report.unchanged.is_empty() {
                    lines.push(format!("MCP unchanged: {}", report.unchanged.join(", ")));
                }
                if !report.connected.is_empty() {
                    let ok: Vec<&str> = report
                        .connected
                        .iter()
                        .filter(|(_, ok)| *ok)
                        .map(|(n, _)| n.as_str())
                        .collect();
                    let fail: Vec<&str> = report
                        .connected
                        .iter()
                        .filter(|(_, ok)| !*ok)
                        .map(|(n, _)| n.as_str())
                        .collect();
                    if !ok.is_empty() {
                        lines.push(format!("MCP connected: {}", ok.join(", ")));
                    }
                    if !fail.is_empty() {
                        lines.push(format!("MCP failed to connect: {}", fail.join(", ")));
                    }
                }
                if !report.removed.is_empty() {
                    lines.push(format!("MCP removed: {}", report.removed.join(", ")));
                }
            }
            lines.push("Re-applied bash policy, hooks, master, and permissions.".to_string());
            record_command_with_duration(
                session,
                resp_tx,
                name,
                args,
                CommandResult::ConfigReload { details: lines },
                Some(start_instant.elapsed().as_millis() as u64),
            )
            .await;
            send_harness_state(resp_tx, &session.id().await, agent, LoopStatus::Idle);
        }
        Some(BuiltinCmd::Tools) => {
            // Handled in TUI (`/tools` opens the tools manager modal
            // locally; it is never forwarded here as a SlashCommand).
        }
        Some(BuiltinCmd::Mcp) => {
            // Handled in TUI: `/mcp` opens the MCP manager modal locally
            // (intercepted in input.rs as `InputAction::OpenMcp`) and is never
            // forwarded here as a SlashCommand. The modal reads the live
            // session-context snapshot, whose MCP pane the harness keeps current
            // via the shared `McpRuntime`.
        }
        Some(BuiltinCmd::Permissions) => {
            if parts.get(1) == Some(&"clear") {
                agent.clear_allowed_tools();
                record_ack(session, name, args, "Always-allowed tool rules cleared.").await;
            } else {
                let allowed = agent.allowed_tools();
                record_command(
                    session,
                    resp_tx,
                    name,
                    args,
                    CommandResult::PermissionList { allowed },
                )
                .await;
            }
        }
        Some(BuiltinCmd::Delegate) => {
            let arg = parts.get(1).map(|s| s.to_lowercase()).unwrap_or_default();
            let next = match parse_delegate_arg(&arg) {
                Ok(next) => next,
                Err(msg) => {
                    record_error(session, resp_tx, name, args, msg).await;
                    return;
                }
            };
            // A bare `/delegate` (`None`) toggles the current state.
            let enabled = next.unwrap_or_else(|| !agent.delegated());
            agent.set_delegated(enabled);
            if let Err(error) = session.set_delegated(enabled).await {
                tracing::warn!(
                    error = %error,
                    "could not persist delegated posture; it will not survive a restart"
                );
            }
            // The ack is a headline plus dimmed explanation lines (never a
            // `•`-joined one-row squeeze); the command entry settles in place
            // with this body, so the mode change owns its own durable row.
            // The current posture is carried by the `DelegatedChanged` chip, not a
            // second transcript entry.
            let (title, detail) = if enabled {
                (
                    "Delegated mode ON",
                    vec![
                        "Autonomous decision-making & tool execution enabled".to_string(),
                        "Ambiguities resolved self-reliantly without interruptions".to_string(),
                    ],
                )
            } else {
                (
                    "Delegated mode OFF",
                    vec![
                        "Interactive confirmation prompts restored".to_string(),
                        "Questions and approval prompts are available".to_string(),
                    ],
                )
            };
            record_command(
                session,
                resp_tx,
                name,
                args,
                CommandResult::Ack {
                    title: title.to_string(),
                    detail: Some(detail.clone()),
                },
            )
            .await;
            // No notice twin: the command entry's Ack body IS the durable,
            // surfaced record of this posture change (ADR-0091/0111); the
            // live posture chip is `DelegatedChanged` below. A second inline
            // notice would double-render the same title + detail.
            let _ = resp_tx.send(round_response(
                &session.id().await,
                RoundEvent::DelegatedChanged(enabled),
            ));
        }
        Some(BuiltinCmd::Master) => {
            // /master <role> — switch the live master role (plan §3.3).
            // Resolves the role onto the current identity, applies the
            // resulting profile (identity preamble, capability scope, operation
            // boundary), and surfaces a confirmation. With no argument, lists
            // the available roles.
            match parts.get(1) {
                None | Some(&"") => {
                    let roles: Vec<&'static str> = muta_contracts::MasterPresetId::ALL
                        .iter()
                        .map(|r| r.as_str())
                        .collect();
                    record_command(
                        session,
                        resp_tx,
                        name,
                        args,
                        CommandResult::Text(format!(
                            "Available master roles: {}. Usage: `/master <role>` or \
                             mention `@master:<role>` in a message.",
                            roles.join(", ")
                        )),
                    )
                    .await;
                }
                Some(role) => match agent.apply_master_role(role) {
                    Some(resolved) => {
                        let _ = session.set_delegated(agent.delegated()).await;
                        let _ = resp_tx.send(round_response(
                            &session.id().await,
                            RoundEvent::DelegatedChanged(agent.delegated()),
                        ));
                        record_command(
                            session,
                            resp_tx,
                            name,
                            args,
                            CommandResult::Text(format!(
                                "Master role switched to `{}` — {}. The next response will \
                                 speak with this role's perspective and capability scope.",
                                resolved.as_str(),
                                resolved.description()
                            )),
                        )
                        .await;
                    }
                    None => {
                        record_error(
                            session,
                            resp_tx,
                            name,
                            args,
                            format!(
                                "Unknown master role `{}`. Available roles: {}.",
                                role,
                                muta_contracts::MasterPresetId::ALL
                                    .iter()
                                    .map(|r| r.as_str())
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            ),
                        )
                        .await;
                    }
                },
            }
        }
        Some(BuiltinCmd::Search) => {
            let query = cmd.strip_prefix("/search").unwrap_or("").trim();
            if query.is_empty() {
                record_command(
                    session,
                    resp_tx,
                    name,
                    args,
                    CommandResult::Text("Usage: /search <query>".to_string()),
                )
                .await;
            } else {
                // Lexical ranking over the live transcript + command ledger.
                // The embedding-index machinery (persisted vectors from a
                // hash-based mock provider) was real cost with no semantics;
                // until a real embedding provider exists, `/search` is
                // deterministic lexical scoring over the in-memory transcript
                // — no index file, no rewrite per search.
                let messages = session.full_transcript().await;
                let commands = session.commands().await;
                let hits = crate::search_lexical::search(query, &messages, &commands, 5);
                let hits = hits
                    .into_iter()
                    .map(|hit| muta_contracts::SearchHit {
                        text: hit.text,
                        score: hit.score,
                    })
                    .collect::<Vec<_>>();
                if hits.is_empty() {
                    record_command(
                        session,
                        resp_tx,
                        name,
                        args,
                        CommandResult::Text("No matching messages in this session.".to_string()),
                    )
                    .await;
                } else {
                    record_command_with_duration(
                        session,
                        resp_tx,
                        name,
                        args,
                        CommandResult::Search {
                            query: query.to_string(),
                            hits,
                        },
                        Some(start_instant.elapsed().as_millis() as u64),
                    )
                    .await;
                }
            }
        }
        Some(BuiltinCmd::Sessions) => {
            // `/sessions` opens the picker; `/sessions <id>` opens that
            // session directly. The retired `/resume` and `/session`
            // spellings resolve here through the alias table, so their legacy
            // grammar is translated first:
            //   `/resume [id]`             → picker, or open <id>
            //   `/session`                  → picker
            //   `/session open|resume <id>`  → open <id> (picker without one)
            //   `/session list`             → picker
            //   `/session new`              → fresh session (same as `/new`)
            //   `/session fork`             → fork (same as `/fork`)
            //   `/session status`           → retired; error with guidance
            let route = match session_route(name, &parts) {
                Ok(route) => route,
                Err(message) => {
                    record_error(session, resp_tx, name, args, message).await;
                    return;
                }
            };
            match route {
                SessionRoute::New => {
                    // Rebuild a SlashEnv whose mutable usage cell points at
                    // the same place, without partially moving `env` while
                    // fields are still read for the rest of the match.
                    let provider_usage = &mut *env.provider_usage;
                    let mut fresh_env = SlashEnv {
                        side: env.side,
                        session: env.session,
                        config: env.config,
                        agent: env.agent,
                        lifecycle: env.lifecycle,
                        resp_tx: env.resp_tx,
                        provider_for_task: env.provider_for_task,
                        provider_usage,
                        mcp_runtime: env.mcp_runtime,
                        workspace_security: env.workspace_security,
                        shared_additional_roots: env.shared_additional_roots,
                        base_tools_for_side: env.base_tools_for_side,
                        skills_registry: env.skills_registry.clone(),
                        skills_registry_for_commands: env.skills_registry_for_commands,
                        _commands_for_task: env._commands_for_task,
                        embedding_store_for_commands: env.embedding_store_for_commands,
                        req_tx_for_commands: env.req_tx_for_commands,
                        project_root_for_side: env.project_root_for_side,
                        startup: env.startup,
                        ui: env.ui,
                        extra_commands: env.extra_commands,
                        websearch_shared: env.websearch_shared,
                    };
                    start_fresh_session(&mut fresh_env, name, args).await;
                }
                SessionRoute::Fork => {
                    fork_current_session(lifecycle, agent, session, side, resp_tx, name, args)
                        .await;
                }
                SessionRoute::Status => {
                    record_error(
                        session,
                        resp_tx,
                        name,
                        args,
                        "/session status is retired. Session id, counts, and timestamps now live \
                         in the /sessions info view (press i in the picker).",
                    )
                    .await;
                }
                SessionRoute::Open(target_id) => match target_id {
                    // `/sessions <id>` (and the legacy `/resume <id>`,
                    // `/session open <id>`, `/session list`) — the same flow
                    // the picker's Enter key drives. Without an id, the
                    // picker (the old "resume most recent" guess is gone).
                    Some(id) => {
                        supersede_for_session_switch(lifecycle, agent, resp_tx).await;
                        teardown_sides_for_session_switch(side, resp_tx).await;
                        match session.open(id).await {
                            Ok(()) => {
                                // Full restore of the session-scoped runtime the
                                // bootstrap skipped in Picker mode: todos,
                                // disabled tools, round counter, and SessionStart
                                // hooks. Opening a prior session is a resume.
                                restore_session_runtime(
                                    session,
                                    agent,
                                    resp_tx,
                                    muta_contracts::SessionSource::Resume,
                                )
                                .await;
                                let transcript = session.full_transcript().await;
                                let _ = resp_tx.send(AgentResponse::ConversationReplaced {
                                    session_id: session.id().await,
                                    messages: transcript,
                                    commands: session.commands().await,
                                    round_interrupts: session.round_interrupts().await,
                                });
                                // The live provider tracks the opened session's
                                // own provider pin (or the global default).
                                crate::handlers_provider::reapply_session_selection(
                                    config,
                                    agent,
                                    provider_for_task,
                                    session,
                                    resp_tx,
                                    provider_usage,
                                )
                                .await;
                                record_command(
                                    session,
                                    resp_tx,
                                    name,
                                    args,
                                    CommandResult::Text(format!(
                                        "Opened session {}.",
                                        short_session_id(&session.id().await)
                                    )),
                                )
                                .await;
                                send_harness_state(
                                    resp_tx,
                                    &session.id().await,
                                    agent,
                                    LoopStatus::Idle,
                                );
                            }
                            Err(error) => {
                                record_error(session, resp_tx, name, args, error).await;
                            }
                        }
                    }
                    None => {
                        // No id (bare `/sessions`, `/session list`, or a
                        // legacy open/resume without one): open the picker.
                        record_invocation(session, name, args).await;
                        let _ = resp_tx.send(AgentResponse::SessionsOverview(
                            build_sessions_overview(session).await,
                        ));
                        let _ = resp_tx.send(AgentResponse::OpenSessionsPanel);
                    }
                },
            }
        }
        Some(BuiltinCmd::Fork) => {
            fork_current_session(lifecycle, agent, session, side, resp_tx, name, args).await;
        }
        Some(BuiltinCmd::Tree) => {
            let tree = session.tree().await;
            let _ = resp_tx.send(AgentResponse::SessionTreeSnapshot {
                session_id: session.id().await,
                tree: tree.clone(),
            });
            let _ = resp_tx.send(AgentResponse::OpenTreePanel);
            record_ack(
                session,
                name,
                args,
                &format!(
                    "Session DAG Tree has {} nodes and {} branch leaves.",
                    tree.entries.len(),
                    tree.leaves().len()
                ),
            )
            .await;
        }
        Some(BuiltinCmd::Diff) => {
            record_ack(
                session,
                name,
                args,
                "Workspace diff tracking is active on the current conversation branch.",
            )
            .await;
        }
        Some(BuiltinCmd::Undo) => {
            let tree = session.tree().await;
            if let Some(current_leaf) = tree.active_leaf()
                && let Some(parent_id) = tree
                    .entries
                    .get(&current_leaf)
                    .and_then(|e| e.parent_id.clone())
            {
                let _ = session.switch_tree_leaf(&parent_id).await;
                record_ack(
                    session,
                    name,
                    args,
                    &format!(
                        "Rolled back active conversation branch to parent node {}.",
                        parent_id
                    ),
                )
                .await;
            } else {
                record_ack(
                    session,
                    name,
                    args,
                    "Cannot undo: already at the root of the conversation tree.",
                )
                .await;
            }
        }
        Some(BuiltinCmd::Dashboard) => {
            record_invocation(session, name, args).await;
            // The session dashboard renders the monitor stream the TUI
            // maintains client-side (ADR-0096); this is only the open signal.
            let _ = resp_tx.send(AgentResponse::OpenHostPanel);
        }
        Some(BuiltinCmd::Usage) => {
            // Handled in TUI: `/usage` opens the usage-statistics overlay
            // locally (intercepted in input.rs as `InputAction::OpenUsage`)
            // and issues `AgentRequest::QueryUsageStats`; it never arrives
            // here as a SlashCommand. Reaching this arm means a non-TUI
            // client (Web app) typed it — answer inline with a pointer so
            // the command is never silently dropped.
            record_ack(
                session,
                name,
                args,
                "Usage statistics are shown by the TUI overlay — open the terminal app and run /usage there.",
            )
            .await;
        }
        Some(BuiltinCmd::Btw) => {
            // `/btw` grammar (ADR-0103 §4):
            //   `/btw`        — open a NEW aside view (no round yet);
            //   `/btw <text>` — open a new aside and auto-send <text> as its
            //                   first turn;
            //   `/btw list`   — open the asides modal (same as F5).
            //
            // Opening forks the primary into a self-contained side file,
            // builds a fresh aside `Agent` + store, and switches the view.
            // The primary round keeps running untouched — unlike
            // `/session open`, we deliberately do NOT bump the generation
            // counter, reject permissions, or cancel the primary token.
            // Existing asides stay live in the registry: each `/btw` creates
            // an additional aside (ADR-0103 lifts ADR-0017's single slot).
            let rest = cmd.strip_prefix("/btw").unwrap_or("").trim();
            if rest == "list" {
                record_invocation(session, name, args).await;
                publish_btw_list(side, resp_tx).await;
                return;
            }
            let prompt = rest;
            let side_session = match SideSession::build(
                session,
                base_tools_for_side,
                provider_for_task,
                Arc::unwrap_or_clone(skills_registry.clone()),
                project_root_for_side,
                agent.identity().clone(),
                agent.workspace_security_handle(),
            )
            .await
            {
                Ok(s) => s,
                Err(error) => {
                    record_error(session, resp_tx, name, args, error).await;
                    return;
                }
            };
            let side_id = side_session.id.clone();
            if let Some(ledger) = agent.token_ledger() {
                side_session.agent.install_token_ledger(ledger.clone());
                ledger.restore_session(&side_id, side_session.store.request_usage_records().await);
            }
            let side_context = side_session
                .agent
                .estimate_next_request_tokens(&side_session.store.model_window().await)
                .total_tokens;
            let _ = resp_tx.send(round_response(
                &side_id,
                RoundEvent::ContextTokens(muta_contracts::ContextTokenSnapshot::new(
                    side_context,
                    muta_contracts::ContextTokenSource::Projection,
                )),
            ));
            // Register + make it the active view, then tell the TUI to enter
            // the aside view — `SideViewOpened` carries the aside's full
            // transcript (inherited parent context included, ADR-0103 §6)
            // and lands before the first aside round starts streaming.
            side.write().await.open(side_session);
            crate::handlers_session::emit_side_view_opened(side, session, resp_tx, &side_id).await;
            publish_btw_list(side, resp_tx).await;
            record_invocation(session, name, args).await;
            // Stream coarse primary-status updates to the aside banner while
            // any aside is live. The watcher spans the whole registry and
            // self-terminates when the last aside closes; spawning a second
            // one while the first is still alive is harmless (both emit only
            // on change and dedupe through the shared last-value cell on the
            // TUI side).
            spawn_parent_status_watcher((*side).clone(), (*lifecycle).clone(), (*resp_tx).clone());
            if !prompt.is_empty() {
                start_active_turn(
                    SideEnv {
                        side,
                        master: agent,
                        primary_session: session,
                        primary_lifecycle: lifecycle,
                        tx: resp_tx,
                        config,
                    },
                    RoundInput {
                        prompt: prompt.to_string(),
                        hidden: false,
                        display_prompt: None,
                        sent_at_ms: None,
                        images: Vec::new(),
                        driver: muta_agent::orchestration::RoundDriver::Fresh,
                    },
                )
                .await;
            }
        }
        Some(BuiltinCmd::Compact) => {
            let mut current = session.model_window().await;
            let settings =
                ContextProjectionSettings::from_config(config, active_context_window(agent))
                    .for_request(agent.estimate_next_request_tokens(&current));
            // No `RoundEvent::Activity` here: a slash command is a
            // control-plane operation outside the round state machine
            // (ADR-0110), and the TUI's activity-bar listener arms
            // `is_responding` on every Activity event — a command must not
            // be able to light the round liveness surface (or overwrite a
            // live round's label). The typed result below ("Compacted N
            // messages …") is the feedback.
            let extra = agent.fire_pre_compact().await;
            match compact_round_history(
                &mut current,
                session,
                &settings,
                Some(agent.provider.clone()),
                extra,
            )
            .await
            {
                Ok(Some(checkpoint)) => {
                    record_invocation(session, name, args).await;
                    send_compaction(resp_tx, &session.id().await, &checkpoint);
                }
                Ok(None) => {
                    record_command(
                        session,
                        resp_tx,
                        name,
                        args,
                        CommandResult::Text("Not enough complete rounds to compact.".to_string()),
                    )
                    .await;
                }
                Err(error) => {
                    record_error(session, resp_tx, name, args, error).await;
                }
            }
            agent.fire_post_compact().await;
        }
        Some(BuiltinCmd::Repeat) => {
            // `/repeat` is retained as a cron-only alias for the unified
            // `/schedule` command. It only accepts a five-field cron expression
            // plus a prompt; for countdown / absolute-time one-shots use
            // `/schedule`. `list` / `cancel` / `help` are shared verbatim.
            let rest = cmd.strip_prefix("/repeat").unwrap_or("").trim();
            if rest.is_empty() || rest == "help" {
                record_command(
                    session,
                    resp_tx,
                    name,
                    args,
                    CommandResult::Text(
                        "Usage: /repeat <cron> <prompt>  (cron-only alias for /schedule)\n\
                         cron is five fields: minute hour day month weekday \
                         (e.g. `*/5 * * * *` = every 5 min, `0 9 * * 1-5` = 09:00 weekdays).\n\
                         For one-shot timers use /schedule <countdown|time> <prompt>.\n\
                         Also: /repeat list, /repeat cancel <id>."
                            .to_string(),
                    ),
                )
                .await;
                return;
            }
            if rest == "list" {
                list_scheduled_jobs(session, resp_tx, name, args).await;
                return;
            }
            if let Some(id) = rest.strip_prefix("cancel ") {
                cancel_scheduled_job(session, id.trim(), resp_tx, name, args).await;
                return;
            }
            // `/repeat <5-field cron> <prompt>` — enforce cron shape.
            let tokens: Vec<&str> = rest.split_whitespace().collect();
            if tokens.len() < 6 {
                record_error(
                    session,
                    resp_tx,
                    name,
                    args,
                    "Usage: /repeat <5-field cron> <prompt>. \
                      Example: /repeat */5 * * * * check the deploy",
                )
                .await;
                return;
            }
            let cron = tokens[0..5].join(" ");
            let prompt = tokens[5..].join(" ");
            if let Err(error) = CronExpr::parse(&cron) {
                record_error(
                    session,
                    resp_tx,
                    name,
                    args,
                    format!(
                        "/repeat takes a cron expression; got '{cron}': {error}. \
                         Use /schedule for countdown / absolute-time one-shots."
                    ),
                )
                .await;
                return;
            }
            add_scheduled_job(
                session,
                &cron,
                &prompt,
                resp_tx,
                req_tx_for_commands,
                name,
                args,
            )
            .await;
        }
        Some(BuiltinCmd::Schedule) => {
            // `/schedule` is the unified scheduled-prompt command. Its time
            // argument is one of:
            //   - a five-field cron expression (recurring),
            //   - a relative countdown (`10m`, `in 2 hours 30 minutes`),
            //   - an absolute time (`14:00`, `tomorrow 09:00`,
            //     `2026-03-15 14:00`).
            // followed by the prompt to run. `list` / `cancel <id>` / `help`
            // are shared with `/repeat`.
            let rest = cmd.strip_prefix("/schedule").unwrap_or("").trim();
            if rest.is_empty() || rest == "help" {
                record_command(
                    session,
                    resp_tx,
                    name,
                    args,
                    CommandResult::Text(
                        "Usage: /schedule <when> <prompt>\n\
                         <when> is one of:\n\
                         - a cron: `*/5 * * * *`, `0 9 * * 1-5`\n\
                         - a countdown: `10m`, `2h30m`, `in 2 hours 30 minutes`\n\
                         - an absolute time: `14:00`, `tomorrow 09:00`, `2026-03-15 14:00`\n\
                         Cron jobs recur; countdown / absolute jobs fire once.\n\
                         Also: /schedule list, /schedule cancel <id>."
                            .to_string(),
                    ),
                )
                .await;
                return;
            }
            if rest == "list" {
                list_scheduled_jobs(session, resp_tx, name, args).await;
                return;
            }
            if let Some(id) = rest.strip_prefix("cancel ") {
                cancel_scheduled_job(session, id.trim(), resp_tx, name, args).await;
                return;
            }
            // Split the time spec from the prompt. The time spec is either:
            //   - exactly five cron fields, or
            //   - everything up to the first run of alphabetic/non-numeric text
            //     that begins the prompt. We detect by: if the first five
            //     whitespace tokens parse as cron, the time spec is those five
            //     fields; otherwise the time spec is the first token (a compact
            //     countdown like `10m` / `2h30m`) OR a small fixed phrase
            //     (`in …`, `today …`, `tomorrow …`, `at …`, `YYYY-MM-DD…`).
            let now = chrono::Utc::now();
            let (time_spec, prompt) = match split_schedule_spec(rest) {
                Some(pair) => pair,
                None => {
                    record_error(
                        session,
                        resp_tx,
                        name,
                        args,
                        "Usage: /schedule <when> <prompt>. \
                         Example: /schedule 10m re-run the tests",
                    )
                    .await;
                    return;
                }
            };
            let prompt = prompt.trim();
            if prompt.is_empty() {
                record_error(
                    session,
                    resp_tx,
                    name,
                    args,
                    "Usage: /schedule <when> <prompt>. The prompt is required.",
                )
                .await;
                return;
            }
            let when = match parse_schedule_arg(&time_spec, now) {
                Some(w) => w,
                None => {
                    record_error(
                        session,
                        resp_tx,
                        name,
                        args,
                        format!(
                            "Could not parse `{time_spec}` as a cron, countdown, or absolute time.\n\
                             Try `*/5 * * * *`, `10m`, `in 2 hours`, `14:00`, `tomorrow 09:00`, \
                             or `2026-03-15 14:00`."
                        ),
                    )
                    .await;
                    return;
                }
            };
            let (trigger, next) = match when.resolve(now) {
                Some(pair) => pair,
                None => {
                    record_error(
                        session,
                        resp_tx,
                        name,
                        args,
                        "That schedule never fires (the time already passed or the cron is \
                         impossible).",
                    )
                    .await;
                    return;
                }
            };
            let mut jobs = session.scheduled_jobs().await;
            let id = uuid::Uuid::new_v4().to_string();
            let short_id = id[..8.min(id.len())].to_string();
            let job = ScheduledJob {
                id: id.clone(),
                trigger: trigger.clone(),
                prompt: prompt.to_string(),
                created_at: now,
                next_fire: next,
                last_fire: None,
            };
            jobs.push(job);
            match session.set_scheduled_jobs(jobs).await {
                Ok(()) => {
                    let kind = trigger.kind_label();
                    let next_str = format!("{}", next.format("%Y-%m-%d %H:%M"));
                    record_command(
                        session,
                        resp_tx,
                        name,
                        args,
                        CommandResult::Scheduled {
                            kind: kind.to_string(),
                            id: short_id,
                            trigger: trigger.display(),
                            next: format!(
                                "{next_str}{}",
                                if trigger.is_once() {
                                    String::new()
                                } else {
                                    " Running now.".to_string()
                                }
                            ),
                        },
                    )
                    .await;
                    // Recurring cron jobs fire the first run immediately (the
                    // scheduler handles the rest); one-shot jobs wait for their
                    // scheduled fire time and are NOT run now.
                    if !trigger.is_once() {
                        let _ = req_tx_for_commands.send(AgentRequest::Prompt {
                            text: prompt.to_string(),
                            images: Vec::new(),
                            sent_at_ms: None,
                        });
                    }
                }
                Err(error) => {
                    record_error(session, resp_tx, name, args, error).await;
                }
            }
        }
        Some(BuiltinCmd::Init) => {
            let target = parts.get(1).copied().unwrap_or(".");
            match init_muta_config(std::path::Path::new(target)) {
                Ok(created) if created.is_empty() => {
                    record_command(
                        session,
                        resp_tx,
                        name,
                        args,
                        CommandResult::Text(format!(
                            "muta is already configured in '{}'. Nothing to do.",
                            target
                        )),
                    )
                    .await;
                }
                Ok(created) => {
                    record_command(
                        session,
                        resp_tx,
                        name,
                        args,
                        CommandResult::Text(format!(
                            "Initialized muta configuration in '{}'.\nCreated:\n{}",
                            target,
                            created
                                .iter()
                                .map(|path| format!("- {}", path))
                                .collect::<Vec<_>>()
                                .join("\n")
                        )),
                    )
                    .await;
                }
                Err(error) => {
                    record_error(session, resp_tx, name, args, error).await;
                }
            }
        }
        Some(BuiltinCmd::Trust) | Some(BuiltinCmd::Untrust) => {
            let route = match trust_route(name, &parts) {
                Ok(route) => route,
                Err(error) => {
                    record_error(session, resp_tx, name, args, error).await;
                    return;
                }
            };
            match route {
                TrustRoute::Status => {
                    let snapshot = workspace_security.snapshot(project_root_for_side);
                    agent.set_workspace_security(snapshot.clone());
                    let message = format!(
                        "Workspace Asset Trust\n\
                         - Root: {}\n\
                         - MCP: {}\n\
                         - Skills: {}\n\
                         - Hooks: {}\n\
                         - Rules: {}\n\
                         - Roots: {}\n\
                         - Aggregate: {}\n\
                         Asset trust does not grant filesystem scope or runtime execution permission beyond declared workspace roots.",
                        snapshot.root,
                        snapshot.mcp.as_str(),
                        snapshot.skills.as_str(),
                        snapshot.hooks.as_str(),
                        snapshot.rules.as_str(),
                        snapshot.roots.as_str(),
                        snapshot.aggregate().as_str(),
                    );
                    record_command(session, resp_tx, name, args, CommandResult::Text(message))
                        .await;
                }
                TrustRoute::GrantAll | TrustRoute::Grant(_) => {
                    let domains: &[TrustDomain] = match route {
                        TrustRoute::GrantAll => &TrustDomain::ALL,
                        TrustRoute::Grant(ref domain) => std::slice::from_ref(domain),
                        _ => unreachable!(),
                    };
                    let granted =
                        match workspace_security.trust_domains(project_root_for_side, domains) {
                            Ok(granted) => granted,
                            Err(error) => {
                                record_error(session, resp_tx, name, args, error).await;
                                return;
                            }
                        };
                    let report = match reload_trusted_assets(
                        agent,
                        mcp_runtime,
                        workspace_security,
                        project_root_for_side,
                        skills_registry_for_commands,
                    )
                    .await
                    {
                        Ok(report) => report,
                        Err(error) => {
                            record_error(session, resp_tx, name, args, error).await;
                            return;
                        }
                    };
                    // Live boundary update: the grant lands on the next
                    // confined tool call — no session restart required.
                    apply_additional_roots(
                        shared_additional_roots,
                        &Config::load(),
                        project_root_for_side,
                        report.snapshot.roots,
                    );
                    let granted = if granted.is_empty() {
                        "none (the selected domains have no project assets)".to_string()
                    } else {
                        granted
                            .iter()
                            .map(|domain| domain.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    };
                    let mcp = if report.connected_mcp.is_empty() {
                        String::new()
                    } else {
                        format!("\n- MCP connected: {}", report.connected_mcp.join(", "))
                    };
                    let message = format!(
                        "Project asset trust recorded.\n\
                         - Root: {}\n\
                         - Granted: {}\n\
                         - MCP: {}; Skills: {}; Hooks: {}; Rules: {}; Roots: {}{}",
                        report.snapshot.root,
                        granted,
                        report.snapshot.mcp.as_str(),
                        report.snapshot.skills.as_str(),
                        report.snapshot.hooks.as_str(),
                        report.snapshot.rules.as_str(),
                        report.snapshot.roots.as_str(),
                        mcp,
                    );
                    record_command(session, resp_tx, name, args, CommandResult::Text(message))
                        .await;
                }
                TrustRoute::Revoke => {
                    let revoked = match workspace_security.revoke_workspace(project_root_for_side) {
                        Ok(revoked) => revoked,
                        Err(error) => {
                            record_error(session, resp_tx, name, args, error).await;
                            return;
                        }
                    };
                    let report = match reload_trusted_assets(
                        agent,
                        mcp_runtime,
                        workspace_security,
                        project_root_for_side,
                        skills_registry_for_commands,
                    )
                    .await
                    {
                        Ok(report) => report,
                        Err(error) => {
                            record_error(session, resp_tx, name, args, error).await;
                            return;
                        }
                    };
                    let removed = if report.removed_mcp.is_empty() {
                        String::new()
                    } else {
                        format!(" Disconnected MCP: {}.", report.removed_mcp.join(", "))
                    };
                    // Live boundary update: collapse admission back to the
                    // primary root immediately — revoke must never wait for a
                    // restart to take effect.
                    apply_additional_roots(
                        shared_additional_roots,
                        &Config::load(),
                        project_root_for_side,
                        report.snapshot.roots,
                    );
                    let message = if revoked {
                        format!(
                            "Project asset trust revoked. MCP, skills, hooks, rules, and project commands were unloaded.{removed}"
                        )
                    } else {
                        "No project asset grants were recorded for this workspace.".to_string()
                    };
                    record_command(session, resp_tx, name, args, CommandResult::Text(message))
                        .await;
                }
            }
            send_harness_state(resp_tx, &session.id().await, agent, LoopStatus::Idle);
        }
        Some(BuiltinCmd::Skills) => {
            let sub = parts.get(1).copied().unwrap_or("list");
            match sub {
                "list" => {
                    let tool = ListSkillsTool {
                        registry: (*skills_registry_for_commands).clone(),
                    };
                    match tool.call("{}").await {
                        Ok(output) => {
                            record_command(
                                session,
                                resp_tx,
                                name,
                                args,
                                CommandResult::Text(output),
                            )
                            .await;
                        }
                        Err(error) => {
                            record_error(session, resp_tx, name, args, error).await;
                        }
                    }
                }
                "reload" => {
                    skills_registry_for_commands.reload().await;
                    let count = skills_registry_for_commands.lock().list().len();
                    record_command(
                        session,
                        resp_tx,
                        name,
                        args,
                        CommandResult::Text(format!(
                            "Skills reloaded. {} skill(s) available.",
                            count
                        )),
                    )
                    .await;
                }
                other => {
                    record_error(
                        session,
                        resp_tx,
                        name,
                        args,
                        format!(
                            "Unknown skills command '{}'. Use 'list' or 'reload'.",
                            other
                        ),
                    )
                    .await;
                }
            }
        }
        Some(BuiltinCmd::Skill) => {
            let skill_name = cmd.strip_prefix("/skill").unwrap_or("").trim();
            if skill_name.is_empty() {
                record_error(session, resp_tx, name, args, "Usage: /skill <name>").await;
            } else {
                let args_json = serde_json::json!({ "name": skill_name }).to_string();
                let tool = UseSkillTool {
                    registry: (*skills_registry_for_commands).clone(),
                };
                match tool.call(&args_json).await {
                    Ok(output) => {
                        record_command(session, resp_tx, name, args, CommandResult::Text(output))
                            .await;
                    }
                    Err(error) => {
                        record_error(session, resp_tx, name, args, error).await;
                    }
                }
            }
        }
        Some(BuiltinCmd::New) => {
            // `/new` never wipes anything in place: it starts a fresh session
            // and leaves the current one on disk (resumable via `/sessions`
            // or `/session open`). The retired `/clear` resolves here through
            // the alias table, so old muscle memory gets the safe semantics.
            start_fresh_session(&mut env, name, args).await;
        }
        Some(BuiltinCmd::Export) => {
            let messages = session.model_window().await;
            let commands = session.commands().await;
            let session_id = session.id().await;
            let provider_id = agent.provider.provider_id();
            let model_name = agent.provider.model();
            let markdown = crate::export::format_export_markdown(
                crate::export::ExportContext {
                    session_id: &session_id,
                    provider: &provider_id,
                    model: &model_name,
                },
                &messages,
                &commands,
            );
            let char_count = markdown.chars().count();
            match ui.copy_to_clipboard(&markdown).await {
                Ok(crate::CopyOutcome::Native) => {
                    record_command(
                        session,
                        resp_tx,
                        name,
                        args,
                        CommandResult::Text(format!(
                            "Session exported to clipboard ({} messages, {} chars). \
                                             Paste it into another agent to continue this work.",
                            messages.len(),
                            char_count
                        )),
                    )
                    .await;
                }
                Ok(crate::CopyOutcome::Osc52) => {
                    record_command(
                        session,
                        resp_tx,
                        name,
                        args,
                        CommandResult::Text(format!(
                            "Session exported via OSC52 ({} messages, {} chars). \
                                             If your terminal did not capture it, run muta in a \
                                             clipboard-capable environment.",
                            messages.len(),
                            char_count
                        )),
                    )
                    .await;
                }
                Err(_) => {
                    let _ = resp_tx.send(AgentResponse::CopyToClipboard { text: markdown });
                    record_command(
                        session,
                        resp_tx,
                        name,
                        args,
                        CommandResult::Text(format!(
                            "Session exported to clipboard ({} messages, {} chars). \
                             Paste it into another agent to continue this work.",
                            messages.len(),
                            char_count
                        )),
                    )
                    .await;
                }
            }
        }
        Some(BuiltinCmd::Debug) => {
            // /debug trace on|off — arm/disarm semantic call tracing at
            // the ProxyProvider layer. Each provider round-trip (request
            // messages + streamed/returned response) is then written as one
            // JSON file under the per-project `network/` directory. Captures
            // the `Vec<Message>` exchange — not raw HTTP bytes — so API keys
            // in headers/query strings never land on disk.
            match parts.get(1).copied() {
                Some("trace") => {
                    let next = match parts.get(2).map(|s| s.to_lowercase()).as_deref() {
                        Some("on") | Some("true") | Some("1") => Some(true),
                        Some("off") | Some("false") | Some("0") => Some(false),
                        Some(other) => {
                            record_error(
                                session,
                                resp_tx,
                                name,
                                args,
                                format!("Unknown value '{other}'. Use `/debug trace on|off`."),
                            )
                            .await;
                            return;
                        }
                        None => None,
                    };
                    let enabled = next.unwrap_or_else(|| !agent.provider.debug_capture_enabled());
                    let dir =
                        muta_persistence::paths::get().project_network_dir(project_root_for_side);
                    agent.provider.set_debug_capture(enabled, dir.clone());
                    record_command(
                        session,
                        resp_tx,
                        name,
                        args,
                        CommandResult::Text(format!(
                            "Trace {}: each provider round-trip {} written to\n  {}",
                            if enabled { "ON" } else { "OFF" },
                            if enabled { "is" } else { "will no longer be" },
                            dir.display(),
                        )),
                    )
                    .await;
                }
                Some("preview") => {
                    // /debug preview — dev-only dry run. Projects the *wire*
                    // body of the next request (the minimal shape the provider
                    // serializes) to disk, with a simulated `This is a test.`
                    // probe user message appended so the snapshot reflects
                    // "what the LLM context would look like if the user sent
                    // this now". Out-of-band fields (nested runner children,
                    // runner_meta, attribution, origin, hidden) are stripped via
                    // `Message::to_wire` — the dump shows what the model
                    // actually sees, not the internal `Message` struct that
                    // also carries durable-session sidecars. NO provider call
                    // is made; nothing is mutated. Reported to the transcript
                    // as a single summary line — the on-disk JSON is the source
                    // of truth for details.
                    let messages = {
                        let mut snapshot = session.model_window().await;
                        // Append the probe BEFORE prepare so it participates in
                        // implicit-skill injection and lands as the final wire
                        // user message the provider would receive.
                        snapshot.push(Message::new(muta_contracts::Role::User, "This is a test."));
                        agent.prepare_request_messages_debug(&mut snapshot);
                        // Project to the wire form: this is what the provider
                        // request body would contain (no children / sidecars).
                        snapshot
                            .into_iter()
                            .map(|m| m.to_wire())
                            .collect::<Vec<_>>()
                    };
                    let provider_id = agent.provider.provider_id();
                    let model_name = agent.provider.model();
                    let window = active_context_window(agent);
                    let tokens = estimate_tokens(&messages);
                    let wire_bytes = messages.iter().map(|m| m.content.len()).sum::<usize>();
                    let session_id = session.id().await;
                    let timestamp = chrono::Utc::now();
                    let pressure_pct = if window > 0 {
                        (tokens as f64 / window as f64 * 100.0).round() as u64
                    } else {
                        0
                    };
                    let n_tools = agent.installed_tools().len();

                    // Persist the full record (raw messages + tool schemas)
                    // for offline inspection.
                    let dir =
                        muta_persistence::paths::get().project_debug_dir(project_root_for_side);
                    let stamp = timestamp.format("%Y%m%d-%H%M%S%.3f");
                    let file = dir.join(format!("{stamp}_preview.json"));
                    let record = serde_json::json!({
                        "timestamp": timestamp.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                        "session_id": session_id,
                        "provider": provider_id,
                        "model": model_name,
                        "context_window_tokens": window,
                        "estimated_tokens": tokens,
                        // wire-size diagnostic: bytes remain the honest unit
                        // for the transport view (ADR-0120 keeps this one).
                        "estimated_wire_bytes": wire_bytes,
                        "pressure_pct": pressure_pct,
                        "tools": agent
                            .installed_tools()
                            .iter()
                            .map(|tool| tool.to_openai_function())
                            .collect::<Vec<_>>(),
                        "messages": messages,
                    });
                    let file_path = file.display().to_string();
                    match serde_json::to_vec_pretty(&record) {
                        Ok(bytes) => {
                            if let Err(error) =
                                muta_persistence::fsutil::atomic_write_bytes(&file, &bytes)
                            {
                                record_error(
                                    session,
                                    resp_tx,
                                    name,
                                    args,
                                    format!("Preview write failed: {error}"),
                                )
                                .await;
                                return;
                            }
                        }
                        Err(error) => {
                            record_error(
                                session,
                                resp_tx,
                                name,
                                args,
                                format!("Preview serialize failed: {error}"),
                            )
                            .await;
                            return;
                        }
                    }

                    let window_str = if window > 0 {
                        format!("of {window} ({pressure_pct}%)")
                    } else {
                        "of unknown window".to_string()
                    };
                    record_command(
                        session,
                        resp_tx,
                        name,
                        args,
                        CommandResult::Text(format!(
                            "Preview (dry run, wire body, probe \"This is a test.\") — \
                             {provider_id}/{model_name}: ~{tokens} tokens {window_str}, {} \
                             message(s), {n_tools} tool(s). Full JSON: {file_path}",
                            messages.len(),
                        )),
                    )
                    .await;
                }
                Some(other) => {
                    record_error(
                        session,
                        resp_tx,
                        name,
                        args,
                        format!(
                            "Unknown debug target '{other}'. Available: trace, preview. \
                             Usage: `/debug trace on|off` or `/debug preview`."
                        ),
                    )
                    .await;
                }
                None => {
                    let trace_on = agent.provider.debug_capture_enabled();
                    record_command(
                        session,
                        resp_tx,
                        name,
                        args,
                        CommandResult::Text(format!(
                            "Debug status:\n- trace: {}\n\nUsage:\n\
                             - `/debug trace on|off` — trace each provider round-trip\n\
                             - `/debug preview` — dry-run the next request to disk",
                            if trace_on { "ON" } else { "OFF" },
                        )),
                    )
                    .await;
                }
            }
        }
        Some(BuiltinCmd::Retry) => {
            if lifecycle.is_running().await {
                record_command(
                    session,
                    resp_tx,
                    name,
                    args,
                    CommandResult::Text(
                        "Cannot retry while a round is already running.".to_string(),
                    ),
                )
                .await;
                return;
            }
            // `/retry` exists to let a stopped round finish itself
            // (ADR-0128): it resumes the parked round with the same number
            // and an unbroken turn sequence. It is deliberately a no-op for
            // a round that completed naturally — re-sending a finished
            // round's history would mint a new round and duplicate the
            // assistant's answer, so without an armed resume point there is
            // simply nothing to do.
            let Some(point) = session.retry_pending().await else {
                record_command(
                    session,
                    resp_tx,
                    name,
                    args,
                    CommandResult::Text(
                        "Nothing to retry — the last round already completed.".to_string(),
                    ),
                )
                .await;
                return;
            };
            // A newer round (or `/new`) retires the point; a stale point for
            // an older round must never resurrect a round the session has
            // moved past.
            if point.round != session.round_counter().await {
                if let Err(error) = session.clear_retry_pending().await {
                    tracing::warn!(%error, "could not clear stale retry point");
                }
                record_command(
                    session,
                    resp_tx,
                    name,
                    args,
                    CommandResult::Text(
                        "Nothing to retry — the last round already completed.".to_string(),
                    ),
                )
                .await;
                return;
            }
            if refuse_if_no_provider(resp_tx, agent, &session.id().await) {
                return;
            }
            record_invocation(session, name, args).await;
            start_active_turn(
                SideEnv {
                    side,
                    master: agent,
                    primary_session: session,
                    primary_lifecycle: lifecycle,
                    tx: resp_tx,
                    config,
                },
                RoundInput::resume(point),
            )
            .await;
        }
        Some(BuiltinCmd::Help) => {
            let live_commands = live_custom_commands(workspace_security, project_root_for_side);
            let custom_help = if live_commands.is_empty() {
                String::new()
            } else {
                let mut commands = live_commands.values().collect::<Vec<_>>();
                commands.sort_by(|left, right| left.name.cmp(&right.name));
                format!(
                    "\n\nCustom commands:\n{}",
                    commands
                        .into_iter()
                        .map(|command| format!(
                            "/{} — {}",
                            command.name,
                            command
                                .description
                                .as_deref()
                                .unwrap_or("Run project command")
                        ))
                        .collect::<Vec<_>>()
                        .join("\n")
                )
            };
            let extra_help = if extra_commands.is_empty() {
                String::new()
            } else {
                format!(
                    "\n\nExtension commands:\n{}",
                    extra_commands
                        .list()
                        .into_iter()
                        .map(|(name, desc)| format!("/{name} — {desc}"))
                        .collect::<Vec<_>>()
                        .join("\n")
                )
            };
            let mut lines = vec!["Slash commands:".to_string()];
            for (name, desc) in BuiltinCmd::ALL {
                lines.push(format!("{name:<13} — {desc}"));
            }
            record_command(
                session,
                resp_tx,
                name,
                args,
                CommandResult::Text(format!("{}\n{custom_help}{extra_help}", lines.join("\n\n"))),
            )
            .await;
        }
        Some(BuiltinCmd::Exit) => {
            record_invocation(session, name, args).await;
            let _ = resp_tx.send(AgentResponse::Exit);
        }
        None => {
            // Rebuild from the live content-bound state on every dispatch.
            // This makes `/trust`, `/untrust`, and a digest mismatch take
            // effect without a restart or stale cache.
            let live_commands = live_custom_commands(workspace_security, project_root_for_side);
            // Application-registered Rust handlers (extension point): try the
            // extra-commands registry before the markdown-template path. A
            // handler that returns `true` fully handled it; `false` falls
            // through to the markdown template / unknown-command path below.
            let name_no_slash = parts[0].strip_prefix('/').unwrap_or(parts[0]);
            if let Some(handler) = extra_commands.get(name_no_slash) {
                let ctx = SlashContext {
                    cmd: &cmd,
                    parts: &parts,
                    config,
                    agent,
                    resp_tx,
                    session,
                    lifecycle,
                    side,
                    base_tools: base_tools_for_side,
                    provider_holder: provider_for_task,
                    provider_usage,
                    skills_registry: skills_registry_for_commands,
                    commands: &live_commands,
                    embedding_store: embedding_store_for_commands,
                    req_tx: req_tx_for_commands,
                    project_root: project_root_for_side,
                    startup,
                    ui,
                };
                if handler.handle(ctx).await {
                    return;
                }
            }
            let (command_name, arguments) = split_custom_command(&cmd);
            let command = live_commands.get(command_name).cloned();
            let Some(command) = command else {
                record_error(
                    session,
                    resp_tx,
                    name,
                    args,
                    format!("Unknown command: {}", parts[0]),
                )
                .await;
                return;
            };
            record_invocation(session, name, args).await;
            start_active_turn(
                SideEnv {
                    side,
                    master: agent,
                    primary_session: session,
                    primary_lifecycle: lifecycle,
                    tx: resp_tx,
                    config,
                },
                RoundInput {
                    prompt: expand_command(&command, arguments),
                    hidden: false,
                    display_prompt: Some(cmd),
                    sent_at_ms: None,
                    images: Vec::new(),
                    driver: muta_agent::orchestration::RoundDriver::Fresh,
                },
            )
            .await;
        }
    }
}

// ── `/schedule` + `/repeat` helpers ───────────────────────────────────────
//
// Shared list / cancel / add paths for the unified scheduled-prompt command.
// `/repeat` is a cron-only alias that funnels into `add_scheduled_job`; the
// `list` / `cancel` sub-commands are identical for both commands.

/// `/schedule list` / `/repeat list`: list every scheduled job sorted by next
/// fire, showing kind, trigger, next-fire, and prompt.
async fn list_scheduled_jobs(
    session: &Arc<SessionStore>,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    name: &str,
    args: &str,
) {
    let mut jobs = session.scheduled_jobs().await;
    if jobs.is_empty() {
        record_command(
            session,
            resp_tx,
            name,
            args,
            CommandResult::ScheduledList {
                entries: Vec::new(),
            },
        )
        .await;
        return;
    }
    jobs.sort_by_key(|j| j.next_fire);
    let mut lines = Vec::new();
    for j in &jobs {
        lines.push(format!(
            "  {} · {} · `{}` · next {} · {}",
            &j.id[..8.min(j.id.len())],
            j.trigger.kind_label(),
            j.trigger.display(),
            j.next_fire.format("%Y-%m-%d %H:%M"),
            j.prompt,
        ));
    }
    record_command(
        session,
        resp_tx,
        name,
        args,
        CommandResult::ScheduledList { entries: lines },
    )
    .await;
}

/// `/schedule cancel <id>` / `/repeat cancel <id>`: drop the job with that id.
async fn cancel_scheduled_job(
    session: &Arc<SessionStore>,
    id: &str,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    name: &str,
    args: &str,
) {
    let mut jobs = session.scheduled_jobs().await;
    let before = jobs.len();
    jobs.retain(|j| j.id != id);
    if before == jobs.len() {
        record_command(
            session,
            resp_tx,
            name,
            args,
            CommandResult::Text(format!("No scheduled job with id {id}.")),
        )
        .await;
        return;
    }
    match session.set_scheduled_jobs(jobs).await {
        Ok(()) => {
            record_command(
                session,
                resp_tx,
                name,
                args,
                CommandResult::Text(format!("Cancelled scheduled job {id}.")),
            )
            .await;
        }
        Err(error) => {
            record_error(session, resp_tx, name, args, error).await;
        }
    }
}

/// `/repeat <cron> <prompt>` shared add path: build a cron `ScheduledJob`,
/// persist it, confirm, and fire the first run immediately (the scheduler
/// handles subsequent firings). The caller validates `cron` is a real cron.
async fn add_scheduled_job(
    session: &Arc<SessionStore>,
    cron: &str,
    prompt: &str,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    req_tx: &mpsc::UnboundedSender<AgentRequest>,
    name: &str,
    args: &str,
) {
    let now = chrono::Utc::now();
    let next = match CronExpr::parse(cron)
        .and_then(|c| c.next_fire(now).ok_or_else(|| "never fires".to_string()))
    {
        Ok(n) => n,
        Err(error) => {
            record_error(
                session,
                resp_tx,
                name,
                args,
                format!("Invalid cron `{cron}`: {error}"),
            )
            .await;
            return;
        }
    };
    let mut jobs = session.scheduled_jobs().await;
    let id = uuid::Uuid::new_v4().to_string();
    let short_id = id[..8.min(id.len())].to_string();
    let job = ScheduledJob {
        id,
        trigger: Schedule::Cron {
            cron: cron.to_string(),
        },
        prompt: prompt.to_string(),
        created_at: now,
        next_fire: next,
        last_fire: None,
    };
    jobs.push(job);
    match session.set_scheduled_jobs(jobs).await {
        Ok(()) => {
            record_command(
                session,
                resp_tx,
                name,
                args,
                CommandResult::Scheduled {
                    kind: "cron".to_string(),
                    id: short_id,
                    trigger: format!("`{cron}`"),
                    next: format!("{} Running now.", next.format("%Y-%m-%d %H:%M")),
                },
            )
            .await;
            // Fire the first run immediately (the scheduler handles the rest).
            let _ = req_tx.send(AgentRequest::Prompt {
                text: prompt.to_string(),
                images: Vec::new(),
                sent_at_ms: None,
            });
        }
        Err(error) => {
            record_error(session, resp_tx, name, args, error).await;
        }
    }
}

/// Parse the argument of `/delegate` (already lowercased by the caller).
///
/// - `""` (bare `/delegate`, no argument) → `Ok(None)`: the dispatch flips
///   the current state, so the command doubles as a toggle.
/// - `on` / `true` / `1` / `delegate` / `auto` / `yolo` → `Ok(Some(true))`
/// - `off` / `false` / `0` → `Ok(Some(false))`
/// - anything else → `Err` with a usage hint.
fn parse_delegate_arg(arg: &str) -> Result<Option<bool>, String> {
    match arg {
        "" => Ok(None),
        "on" | "true" | "1" | "delegate" | "auto" | "yolo" => Ok(Some(true)),
        "off" | "false" | "0" => Ok(Some(false)),
        other => Err(format!(
            "Unknown value '{other}'. Use `/delegate` to toggle, or `/delegate on|off`."
        )),
    }
}

/// Split a `/schedule <when> <prompt>` argument string into `(time_spec,
/// prompt)`. Returns `None` when no prompt follows the time spec.
///
/// The time spec is one of:
/// - the first five whitespace tokens, when they parse as a cron expression;
/// - a leading phrase beginning with `in `, `today`, `tomorrow`, or `at `
///   (consumed up to the first token that does not look like a clock time or
///   a `<number><unit>` continuation);
/// - a single leading compact token for everything else
///   (`10m`, `2h30m`, `14:00`, `2026-03-15T14:00`).
fn split_schedule_spec(rest: &str) -> Option<(String, String)> {
    let tokens: Vec<&str> = rest.split_whitespace().collect();
    if tokens.len() < 2 {
        return None;
    }

    // Five-field cron: spec = first five tokens, prompt = the rest.
    if tokens.len() >= 6 {
        let maybe_cron = tokens[0..5].join(" ");
        if CronExpr::parse(&maybe_cron).is_ok() {
            return Some((maybe_cron, tokens[5..].join(" ")));
        }
    }

    let first = tokens[0];
    let first_lower = first.to_ascii_lowercase();

    // Phrase specs (`in …`, `today …`, `tomorrow …`, `at …`): consume the
    // leading word plus any following time/countdown tokens until the prompt
    // begins. The prompt begins at the first remaining alphabetic word that is
    // not itself a time token. Heuristic: keep consuming while tokens look like
    // `<number>`, `<unit>` (m/min/h/...), or a clock time / ISO date.
    if first_lower == "in"
        || first_lower == "today"
        || first_lower == "tomorrow"
        || first_lower == "at"
    {
        // Find the boundary: the first token after the phrase head that does
        // NOT look like a number, a unit, or a clock/date.
        let mut end = 1; // include the head word
        while end < tokens.len() {
            let tok = tokens[end];
            if looks_like_time_token(tok) {
                end += 1;
            } else {
                break;
            }
        }
        if end >= tokens.len() {
            return None; // no prompt left
        }
        return Some((tokens[..end].join(" "), tokens[end..].join(" ")));
    }

    // Otherwise the spec is the single first token (compact countdown or bare
    // clock time / ISO date), and the rest is the prompt.
    Some((first.to_string(), tokens[1..].join(" ")))
}

/// `true` if `tok` could be part of a time spec: a number, a unit word, a
/// clock time (`HH:MM[:SS]`), or an ISO date/time (`YYYY-MM-DD[T…]`).
fn looks_like_time_token(tok: &str) -> bool {
    if tok.chars().all(|c| c.is_ascii_digit()) {
        return true;
    }
    let l = tok.to_ascii_lowercase();
    matches!(
        l.as_str(),
        "s" | "sec"
            | "secs"
            | "second"
            | "seconds"
            | "m"
            | "min"
            | "mins"
            | "minute"
            | "minutes"
            | "h"
            | "hr"
            | "hrs"
            | "hour"
            | "hours"
            | "d"
            | "day"
            | "days"
    ) || tok.contains(':')
        || (tok.contains('-') && tok.chars().next().is_some_and(|c| c.is_ascii_digit()))
}

#[cfg(test)]
mod schedule_spec_tests {
    use super::split_schedule_spec;

    #[test]
    fn splits_cron_spec() {
        let (spec, prompt) = split_schedule_arg("*/5 * * * * run the tests");
        assert_eq!(spec, "*/5 * * * *");
        assert_eq!(prompt, "run the tests");
    }

    #[test]
    fn splits_compact_countdown() {
        let (spec, prompt) = split_schedule_arg("10m re-run the tests");
        assert_eq!(spec, "10m");
        assert_eq!(prompt, "re-run the tests");
    }

    #[test]
    fn splits_verbose_countdown() {
        let (spec, prompt) = split_schedule_arg("in 2 hours 30 minutes do the thing");
        assert_eq!(spec, "in 2 hours 30 minutes");
        assert_eq!(prompt, "do the thing");
    }

    #[test]
    fn splits_absolute_clock() {
        let (spec, prompt) = split_schedule_arg("14:00 ship the build");
        assert_eq!(spec, "14:00");
        assert_eq!(prompt, "ship the build");
    }

    #[test]
    fn splits_tomorrow_phrase() {
        let (spec, prompt) = split_schedule_arg("tomorrow 09:00 morning standup");
        assert_eq!(spec, "tomorrow 09:00");
        assert_eq!(prompt, "morning standup");
    }

    /// Convenience wrapper so the tests read like the public parse path.
    /// Propagates errors with `expect` — an earlier `unwrap_or(("",""))`
    /// here meant a parse regression turned every one of these tests into
    /// a vacuous green pass on empty strings.
    fn split_schedule_arg(rest: &str) -> (String, String) {
        split_schedule_spec(rest).expect("schedule spec must parse")
    }

    #[test]
    fn missing_prompt_is_none_not_empty_strings() {
        // A spec with no prompt must be `None` (surfaced as a usage notice),
        // never `(spec, "")` — an empty prompt would create a scheduled job
        // that fires with nothing to send.
        assert!(split_schedule_spec("10m").is_none());
        assert!(split_schedule_spec("14:00").is_none());
        assert!(split_schedule_spec("").is_none());
    }
}

#[cfg(test)]
mod session_route_tests {
    use super::{SessionRoute, session_route};

    fn parts(cmd: &str) -> Vec<&str> {
        cmd.split_whitespace().collect()
    }

    #[test]
    fn canonical_sessions_forms() {
        assert_eq!(
            session_route("sessions", &parts("/sessions")),
            Ok(SessionRoute::Open(None))
        );
        assert_eq!(
            session_route("sessions", &parts("/sessions abc123")),
            Ok(SessionRoute::Open(Some("abc123")))
        );
    }

    #[test]
    fn legacy_resume_keeps_its_id_slot() {
        assert_eq!(
            session_route("resume", &parts("/resume")),
            Ok(SessionRoute::Open(None))
        );
        assert_eq!(
            session_route("resume", &parts("/resume abc123")),
            Ok(SessionRoute::Open(Some("abc123")))
        );
    }

    #[test]
    fn legacy_session_subcommands_translate() {
        // The id sits one slot right in the legacy spelling.
        assert_eq!(
            session_route("session", &parts("/session open abc123")),
            Ok(SessionRoute::Open(Some("abc123")))
        );
        assert_eq!(
            session_route("session", &parts("/session resume abc123")),
            Ok(SessionRoute::Open(Some("abc123")))
        );
        // Without an id these fall back to the picker.
        assert_eq!(
            session_route("session", &parts("/session")),
            Ok(SessionRoute::Open(None))
        );
        assert_eq!(
            session_route("session", &parts("/session open")),
            Ok(SessionRoute::Open(None))
        );
        assert_eq!(
            session_route("session", &parts("/session list")),
            Ok(SessionRoute::Open(None))
        );
        assert_eq!(
            session_route("session", &parts("/session new")),
            Ok(SessionRoute::New)
        );
        assert_eq!(
            session_route("session", &parts("/session fork")),
            Ok(SessionRoute::Fork)
        );
        assert_eq!(
            session_route("session", &parts("/session status")),
            Ok(SessionRoute::Status)
        );
    }

    #[test]
    fn unknown_legacy_subcommand_is_an_error() {
        let err = session_route("session", &parts("/session frobnicate")).unwrap_err();
        assert!(
            err.contains("/session is retired"),
            "error should steer away from the retired command: {err}"
        );
    }
}

#[cfg(test)]
mod trust_route_tests {
    use super::{TrustRoute, trust_route};
    use muta_contracts::TrustDomain;

    fn parts(command: &str) -> Vec<&str> {
        command.split_whitespace().collect()
    }

    #[test]
    fn canonical_trust_grammar_is_closed() {
        assert_eq!(
            trust_route("trust", &parts("/trust")),
            Ok(TrustRoute::GrantAll)
        );
        assert_eq!(
            trust_route("trust", &parts("/trust all")),
            Ok(TrustRoute::GrantAll)
        );
        assert_eq!(
            trust_route("trust", &parts("/trust mcp")),
            Ok(TrustRoute::Grant(TrustDomain::Mcp))
        );
        assert_eq!(
            trust_route("trust", &parts("/trust skills")),
            Ok(TrustRoute::Grant(TrustDomain::Skills))
        );
        assert_eq!(
            trust_route("trust", &parts("/trust hooks")),
            Ok(TrustRoute::Grant(TrustDomain::Hooks))
        );
        assert_eq!(
            trust_route("trust", &parts("/trust rules")),
            Ok(TrustRoute::Grant(TrustDomain::Rules))
        );
        assert_eq!(
            trust_route("trust", &parts("/trust status")),
            Ok(TrustRoute::Status)
        );
        assert_eq!(
            trust_route("trust", &parts("/trust revoke")),
            Ok(TrustRoute::Revoke)
        );
        assert_eq!(
            trust_route("untrust", &parts("/untrust")),
            Ok(TrustRoute::Revoke)
        );
    }

    #[test]
    fn retired_or_ambiguous_trust_spellings_are_rejected() {
        for command in [
            "/trust workspace",
            "/trust extensions",
            "/trust readonly",
            "/trust yes",
            "/untrust mcp",
        ] {
            let parsed = parts(command);
            let name = parsed[0].trim_start_matches('/');
            assert!(
                trust_route(name, &parsed).is_err(),
                "{command} must be rejected"
            );
        }
    }
}

#[cfg(test)]
mod delegate_arg_tests {
    use super::parse_delegate_arg;

    #[test]
    fn bare_argument_means_toggle() {
        assert_eq!(parse_delegate_arg(""), Ok(None));
    }

    #[test]
    fn on_forms_enable() {
        assert_eq!(parse_delegate_arg("on"), Ok(Some(true)));
        assert_eq!(parse_delegate_arg("true"), Ok(Some(true)));
        assert_eq!(parse_delegate_arg("1"), Ok(Some(true)));
        assert_eq!(parse_delegate_arg("delegate"), Ok(Some(true)));
        assert_eq!(parse_delegate_arg("auto"), Ok(Some(true)));
        assert_eq!(parse_delegate_arg("yolo"), Ok(Some(true)));
    }

    #[test]
    fn off_forms_disable() {
        assert_eq!(parse_delegate_arg("off"), Ok(Some(false)));
        assert_eq!(parse_delegate_arg("false"), Ok(Some(false)));
        assert_eq!(parse_delegate_arg("0"), Ok(Some(false)));
    }

    #[test]
    fn unknown_value_is_an_error_with_a_usage_hint() {
        let err = parse_delegate_arg("maybe").unwrap_err();
        assert!(
            err.contains("`/delegate` to toggle") && err.contains("`/delegate on|off`"),
            "usage hint missing the toggle form: {err}"
        );
    }
}
