//! The `AgentRequest::SlashCommand` dispatcher, extracted verbatim from the
//! agent background task's `match req { … }` dispatch.
//!
//! This is the largest handler — it fans the parsed command out across every
//! `BuiltinCmd` variant (`/models`, `/mcp`, `/compact`, `/clear`,
//! `/permissions`, `/autopilot`, `/review`, `/search`, `/resume`,
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
use crate::project::init_neenee_config;
use neenee_agent::Agent;
use neenee_agent::RoundLifecycle;
use neenee_agent::mcp::McpRuntime;
use neenee_agent::orchestration::{
    ContextProjectionSettings, RoundInput, compact_round_history, round_response, send_compaction,
    send_harness_state,
};
use neenee_core::{
    AgentNotice, AgentRequest, AgentResponse, CommandRecord, CommandResult, CronExpr, LoopStatus,
    Message, NoticeKind, NoticeSeverity, NoticeSource, NoticeSurface, Provider, RoundEvent,
    Schedule, ScheduledJob, Tool, estimate_bytes, estimate_tokens, repeat::parse_schedule_arg,
};
use neenee_persistence::{
    config::Config, embedding, provider_usage::ProviderUsage, session::SessionStore,
    trusted_projects::TrustGate,
};
use neenee_skills::{ListSkillsTool, SkillRegistry, UseSkillTool};

use std::collections::HashMap;
use std::sync::{
    Arc, RwLock,
    atomic::{AtomicBool, Ordering},
};
use tokio::sync::{RwLock as AsyncRwLock, mpsc};

use crate::agent_setup::active_context_window;
use crate::session_view::{build_sessions_overview, resume_session, short_session_id};
use crate::side::{SideSession, spawn_parent_status_watcher, start_active_turn};
use crate::slash_handler::{SlashCommandRegistry, SlashContext};
use crate::startup::{BuiltinCmd, StartupMode, split_custom_command};

async fn supersede_for_session_switch(
    lifecycle: &RoundLifecycle,
    agent: &Agent,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
) {
    lifecycle.supersede();
    agent.reject_pending_permissions();
    agent.reject_pending_user_questions();
    agent.reject_pending_inputs();
    let _ = resp_tx.send(AgentResponse::PermissionsCleared);
    lifecycle.cancel_current().await;
}

/// Full session-scoped runtime restore, run after the session store has been
/// repointed at a different session (via `/session open`, `/resume`, …).
///
/// This mirrors the restore block the bootstrap skips in Picker mode
/// (`neenee resume` with no id): the unified task list, the disabled-tool
/// mask, the round counter, and the SessionStart hooks. `fire_session_start`
/// lives only in `bootstrap` otherwise, so a session chosen from the startup
/// picker would otherwise never receive its hook-injected setup context.
///
/// `source` is surfaced to SessionStart hooks (`Startup` vs `Resume`) so a
/// hook can branch — opening a prior session from the picker is a resume.
///
/// Emits the restored todos as a `TodosUpdated` event so the frontend's sticky
/// panel appears the moment the user enters the picked session.
async fn restore_session_runtime(
    session: &Arc<SessionStore>,
    agent: &Arc<Agent>,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    source: neenee_core::SessionSource,
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
    let record = CommandRecord::new(name, args).with_result(result.clone());
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
    if let Err(error) = session
        .mutate_commands(|c| c.push(CommandRecord::new(name, args)))
        .await
    {
        tracing::warn!(?error, name, "could not persist command invocation");
    }
}

/// Record an acknowledgment — the durable twin of an ADR-0088 `CommandAck`
/// toast. The live surface stays the toast; the ledger keeps the confirmation
/// for resume/export/audit. No `CommandResult` event is emitted, so a command
/// block never double-renders the toast.
async fn record_ack(session: &Arc<SessionStore>, name: &str, args: &str, title: impl Into<String>) {
    let record = CommandRecord::new(name, args).with_result(CommandResult::Ack {
        title: title.into(),
    });
    if let Err(error) = session.mutate_commands(|c| c.push(record)).await {
        tracing::warn!(?error, name, "could not persist command ack");
    }
}

/// Record a failed slash-command invocation and surface the error. The error
/// keeps its existing `AgentResponse::Error` surface; the ledger records the
/// failure (status `Error`) for audit.
async fn record_error(
    session: &Arc<SessionStore>,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    name: &str,
    args: &str,
    message: impl Into<String>,
) {
    let message = message.into();
    let record = CommandRecord::new(name, args).with_error(message.clone(), None);
    if let Err(error) = session.mutate_commands(|c| c.push(record)).await {
        tracing::warn!(?error, name, "could not persist command error record");
    }
    let _ = resp_tx.send(AgentResponse::Error(message));
}

/// `AgentRequest::SlashCommand` — parse the command, dispatch to the matching
/// built-in handler, or fall through to the user-defined project-command path.
#[allow(clippy::too_many_arguments)]
pub async fn dispatch(
    cmd: String,
    config: &Config,
    agent: &Arc<Agent>,
    mcp_runtime: &Arc<McpRuntime>,
    trust_gate: &Arc<TrustGate>,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    session: &Arc<SessionStore>,
    lifecycle: &Arc<RoundLifecycle>,
    side: &Arc<AsyncRwLock<Option<SideSession>>>,
    active_view_side: &AtomicBool,
    base_tools_for_side: &Arc<Vec<Arc<dyn Tool>>>,
    provider_for_task: &Arc<RwLock<Arc<dyn Provider>>>,
    provider_usage: &mut ProviderUsage,
    skills_registry: Arc<SkillRegistry>,
    skills_registry_for_commands: &Arc<SkillRegistry>,
    commands_for_task: &HashMap<String, CustomCommand>,
    embedding_store_for_commands: &Arc<AsyncRwLock<embedding::EmbeddingStore>>,
    req_tx_for_commands: &mpsc::UnboundedSender<AgentRequest>,
    project_root_for_side: &std::path::Path,
    startup: &StartupMode,
    ui: &dyn crate::UiBridge,
    extra_commands: &SlashCommandRegistry,
) {
    let parts: Vec<&str> = cmd.split_whitespace().collect();
    if parts.is_empty() {
        return;
    }
    // The ledger identity of this invocation (ADR-0091): command word without
    // the leading slash, plus the raw argument remainder.
    let name = parts[0].trim_start_matches('/');
    let args = cmd.strip_prefix(parts[0]).unwrap_or("").trim();
    match BuiltinCmd::from_slash(parts[0]) {
        Some(BuiltinCmd::Models) | Some(BuiltinCmd::Connections) => {
            // Handled in TUI
        }
        Some(BuiltinCmd::Config) => {
            // Handled in the TUI: `/config` opens the config manager modal
            // locally for presentation settings (intercepted in input.rs as
            // `InputAction::OpenConfig`), so it is never forwarded here as a
            // SlashCommand.
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
        Some(BuiltinCmd::Autopilot) => {
            let arg = parts.get(1).map(|s| s.to_lowercase()).unwrap_or_default();
            let next = match parse_autopilot_arg(&arg) {
                Ok(next) => next,
                Err(msg) => {
                    record_error(session, resp_tx, name, args, msg).await;
                    return;
                }
            };
            // A bare `/autopilot` (`None`) toggles the current state.
            let enabled = next.unwrap_or_else(|| !agent.get_autopilot());
            agent.set_autopilot(enabled);
            // The autopilot toggle's confirmation is a command acknowledgment,
            // not model output: surface it as a transient toast rather than
            // appending a same-color line to the transcript (ADR-0088). The
            // ledger still records the `Ack` so the confirmation is durable
            // (ADR-0091). The `AutopilotChanged` event below still refreshes
            // the badge so the new state stays visible long after the toast
            // fades.
            let ack = format!(
                "Autopilot {}: the agent {} run without human intervention — the question \
                     tool is reclaimed, tool permissions auto-approve, and no prompts or \
                     questions can pause the session.",
                if enabled { "ON" } else { "OFF" },
                if enabled { "will" } else { "won't" },
            );
            record_ack(session, name, args, ack.clone()).await;
            let _ = resp_tx.send(round_response(
                &session.id().await,
                RoundEvent::Notice(AgentNotice::command_ack(ack)),
            ));
            let _ = resp_tx.send(round_response(
                &session.id().await,
                RoundEvent::AutopilotChanged(enabled),
            ));
            // No `send_harness_state` here: toggling autopilot is not a
            // round lifecycle transition, so emitting a `HarnessState("idle")`
            // would make the HarnessState handler clear the live activity
            // cell (`activity_status`) and momentarily hide the activity bar
            // mid-turn. The `AutopilotChanged` event above already mirrors
            // the new value into the TUI snapshot without that side effect.
        }
        Some(BuiltinCmd::Principal) => {
            // /principal <role> — switch the live principal role (plan §3.3).
            // Resolves the role onto the current identity, applies the
            // resulting profile (identity preamble, capability scope, operation
            // boundary), and surfaces a confirmation. With no argument, lists
            // the available roles.
            match parts.get(1) {
                None | Some(&"") => {
                    let roles: Vec<&'static str> = neenee_core::PrincipalRole::ALL
                        .iter()
                        .map(|r| r.as_str())
                        .collect();
                    record_command(
                        session,
                        resp_tx,
                        name,
                        args,
                        CommandResult::Text(format!(
                            "Available principal roles: {}. Usage: `/principal <role>` or \
                             mention `@principal:<role>` in a message.",
                            roles.join(", ")
                        )),
                    )
                    .await;
                }
                Some(role) => match agent.apply_principal_role(role) {
                    Some(resolved) => {
                        record_command(
                            session,
                            resp_tx,
                            name,
                            args,
                            CommandResult::Text(format!(
                                "Principal role switched to `{}` — {}. The next response will \
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
                                "Unknown principal role `{}`. Available roles: {}.",
                                role,
                                neenee_core::PrincipalRole::ALL
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
        Some(BuiltinCmd::Review) => {
            // /review — on-demand session review (ADR-0018,
            // superseding the periodic ADR-0016 design).
            // Runs the bounded read-only REVIEW envoy
            // against the current transcript and reports the
            // verdict(s). Review no longer fires on a round
            // schedule; it only runs when asked. Takes no
            // arguments.
            if parts.iter().skip(1).any(|t| !t.trim().is_empty()) {
                record_error(
                    session,
                    resp_tx,
                    name,
                    args,
                    "`/review` takes no arguments. Usage: `/review` runs an \
                                     on-demand diagnostic of the current round.",
                )
                .await;
                return;
            }
            let transcript = session.full_transcript().await;
            let turns = Agent::estimate_completed_turns(&transcript);
            if turns == 0 {
                record_command(
                    session,
                    resp_tx,
                    name,
                    args,
                    CommandResult::Text(
                        "Nothing to review yet — no ReAct turns in the current \
                         round."
                            .to_string(),
                    ),
                )
                .await;
                return;
            }
            let _ = resp_tx.send(round_response(
                &session.id().await,
                RoundEvent::Activity("running session review…".to_string()),
            ));
            let verdicts = agent.review_now(&transcript).await;
            // Mirror the worst verdict into the activity-bar
            // banner (empty alert clears it when healthy).
            let alert = Agent::render_review_alert(&verdicts, turns);
            if !alert.trim().is_empty() {
                let _ = resp_tx.send(round_response(
                    &session.id().await,
                    RoundEvent::Notice(
                        AgentNotice::new(
                            NoticeKind::ReviewAlert,
                            NoticeSeverity::Warning,
                            "Session review needs attention",
                            NoticeSource::Review,
                        )
                        .with_body(alert.clone())
                        .with_surface(NoticeSurface::Banner),
                    ),
                ));
            }
            let _ = resp_tx.send(round_response(
                &session.id().await,
                RoundEvent::SessionReview { alert },
            ));
            // The typed review result is both the durable record and the
            // command-block reply (ADR-0091); the verdicts render through
            // `CommandResult::to_text`, which mirrors `format_review_report`.
            record_command(
                session,
                resp_tx,
                name,
                args,
                CommandResult::Review {
                    verdicts,
                    turns: turns as u64,
                },
            )
            .await;
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
                let messages = session.full_transcript().await;
                {
                    let mut store = embedding_store_for_commands.write().await;
                    let session_id = session.id().await;
                    if let Err(error) = store.index(&messages, &session_id).await {
                        record_error(session, resp_tx, name, args, error).await;
                        return;
                    }
                }
                match embedding_store_for_commands
                    .read()
                    .await
                    .search(query, 5)
                    .await
                {
                    Ok(results) => {
                        let hits = results
                            .into_iter()
                            .map(|(text, score)| neenee_core::SearchHit { text, score })
                            .collect::<Vec<_>>();
                        record_command(
                            session,
                            resp_tx,
                            name,
                            args,
                            CommandResult::Search {
                                query: query.to_string(),
                                hits,
                            },
                        )
                        .await;
                    }
                    Err(error) => {
                        record_error(session, resp_tx, name, args, error).await;
                    }
                }
            }
        }
        Some(BuiltinCmd::Resume) => {
            supersede_for_session_switch(lifecycle, agent, resp_tx).await;
            match resume_session(session, parts.get(1).copied()).await {
                Ok((id, transcript)) => {
                    // Full restore: todos, disabled tools, round counter, and
                    // SessionStart hooks. `/resume` is a resume.
                    restore_session_runtime(
                        session,
                        agent,
                        resp_tx,
                        neenee_core::SessionSource::Resume,
                    )
                    .await;
                    let _ = resp_tx.send(AgentResponse::ConversationReplaced {
                        session_id: session.id().await,
                        messages: transcript,
                        commands: session.commands().await,
                    });
                    record_command(
                        session,
                        resp_tx,
                        name,
                        args,
                        CommandResult::Text(format!("Resumed session {}.", short_session_id(&id))),
                    )
                    .await;
                    send_harness_state(resp_tx, &session.id().await, agent, LoopStatus::Idle);
                }
                Err(error) => {
                    record_error(session, resp_tx, name, args, error).await;
                }
            }
        }
        Some(BuiltinCmd::Session) => match parts.get(1).copied().unwrap_or("status") {
            "status" => {
                let id = session.id().await;
                let parent_id = session.parent_id().await;
                let message_count = session.model_window().await.len();
                let archived_count = session.archived_transcript_count().await;
                let last_projection = session.last_projection().await;
                record_command(
                    session,
                    resp_tx,
                    name,
                    args,
                    CommandResult::SessionStatus {
                        id,
                        parent_id,
                        message_count,
                        archived_count,
                        last_projection: last_projection.map(|item| {
                            format!(
                                "{:?}: {} -> {} chars",
                                item.operation, item.before_chars, item.after_chars
                            )
                        }),
                    },
                )
                .await;
            }
            "list" => match session.list().await {
                Ok(sessions) => {
                    let lines = sessions
                        .into_iter()
                        .map(|item| {
                            format!(
                                "- {}{}  messages={}  parent={}",
                                short_session_id(&item.id),
                                if item.active { " [active]" } else { "" },
                                item.message_count,
                                item.parent_id
                                    .map(|id| short_session_id(&id).to_string())
                                    .unwrap_or_else(|| "none".to_string())
                            )
                        })
                        .collect::<Vec<_>>();
                    record_command(
                        session,
                        resp_tx,
                        name,
                        args,
                        CommandResult::Text(format!("Sessions:\n{}", lines.join("\n"))),
                    )
                    .await;
                }
                Err(error) => {
                    record_error(session, resp_tx, name, args, error).await;
                }
            },
            "fork" => {
                supersede_for_session_switch(lifecycle, agent, resp_tx).await;
                match session.fork().await {
                    Ok((id, parent_id)) => {
                        agent.restore_round_count(session.round_counter().await);
                        record_command(
                            session,
                            resp_tx,
                            name,
                            args,
                            CommandResult::Text(format!(
                                "Forked session {} from {}.",
                                id, parent_id
                            )),
                        )
                        .await;
                        send_harness_state(resp_tx, &session.id().await, agent, LoopStatus::Idle);
                    }
                    Err(error) => {
                        record_error(session, resp_tx, name, args, error).await;
                    }
                }
            }
            "open" => {
                let Some(id) = parts.get(2) else {
                    record_error(
                        session,
                        resp_tx,
                        name,
                        args,
                        "Usage: /session open <session-id>",
                    )
                    .await;
                    return;
                };
                supersede_for_session_switch(lifecycle, agent, resp_tx).await;
                match session.open(id).await {
                    Ok(()) => {
                        // Full restore of the session-scoped runtime the
                        // bootstrap skipped in Picker mode: todos, disabled
                        // tools, round counter, and SessionStart hooks.
                        // Opening a prior session is a resume.
                        restore_session_runtime(
                            session,
                            agent,
                            resp_tx,
                            neenee_core::SessionSource::Resume,
                        )
                        .await;
                        let transcript = session.full_transcript().await;
                        let _ = resp_tx.send(AgentResponse::ConversationReplaced {
                            session_id: session.id().await,
                            messages: transcript,
                            commands: session.commands().await,
                        });
                        // C6: the live provider tracks the opened session's own
                        // provider pin (or the global default if it has none).
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
                            CommandResult::Text(format!("Opened session {}.", id)),
                        )
                        .await;
                        send_harness_state(resp_tx, &session.id().await, agent, LoopStatus::Idle);
                    }
                    Err(error) => {
                        record_error(session, resp_tx, name, args, error).await;
                    }
                }
            }
            "resume" => {
                supersede_for_session_switch(lifecycle, agent, resp_tx).await;
                match resume_session(session, parts.get(2).copied()).await {
                    Ok((id, transcript)) => {
                        // Full restore: todos, disabled tools, round counter,
                        // and SessionStart hooks (`/resume` is a resume).
                        restore_session_runtime(
                            session,
                            agent,
                            resp_tx,
                            neenee_core::SessionSource::Resume,
                        )
                        .await;
                        let _ = resp_tx.send(AgentResponse::ConversationReplaced {
                            session_id: session.id().await,
                            messages: transcript,
                            commands: session.commands().await,
                        });
                        // C6: the live provider tracks the resumed session's own
                        // provider pin (or the global default if it has none).
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
                                "Resumed session {}.",
                                short_session_id(&id)
                            )),
                        )
                        .await;
                        send_harness_state(resp_tx, &session.id().await, agent, LoopStatus::Idle);
                    }
                    Err(error) => {
                        record_error(session, resp_tx, name, args, error).await;
                    }
                }
            }
            "new" => {
                supersede_for_session_switch(lifecycle, agent, resp_tx).await;
                agent.clear_todos();
                match session.reset().await {
                    Ok(id) => {
                        agent.restore_round_count(session.round_counter().await);
                        // C6: a fresh session has no provider pin, so the live
                        // provider falls back to the global default.
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
                            RoundEvent::TodosUpdated(neenee_core::TodoList::default()),
                        ));
                        let _ = resp_tx.send(AgentResponse::ConversationCleared);
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
            other => {
                record_error(
                    session,
                    resp_tx,
                    name,
                    args,
                    format!(
                        "Unknown session command '{}'. Use status, list, resume, fork, open, or new.",
                        other
                    ),
                )
                .await;
            }
        },
        Some(BuiltinCmd::Sessions) => {
            record_invocation(session, name, args).await;
            let _ = resp_tx.send(AgentResponse::SessionsOverview(
                build_sessions_overview(session).await,
            ));
        }
        Some(BuiltinCmd::Dashboard) => {
            record_invocation(session, name, args).await;
            // The session dashboard renders the monitor stream the TUI
            // maintains client-side (ADR-0096); this is only the open signal.
            let _ = resp_tx.send(AgentResponse::OpenHostPanel);
        }
        Some(BuiltinCmd::Btw) => {
            // `/btw [prompt]` opens a side conversation
            // (ADR-0017): fork the primary into a
            // self-contained side file, build a fresh side
            // `Agent` + store + history, and switch the view.
            // The primary round keeps running untouched —
            // unlike `/session open`, we deliberately do NOT
            // bump the generation counter, reject permissions,
            // or cancel the primary token.
            let prompt = cmd.strip_prefix("/btw").unwrap_or("").trim();
            if side.read().await.is_some() {
                record_error(
                    session,
                    resp_tx,
                    name,
                    args,
                    "A side conversation is already open. \
                                     Leave it with Esc first.",
                )
                .await;
                return;
            }
            let primary_id = session.id().await;
            let side_session = match SideSession::build(
                session,
                base_tools_for_side,
                provider_for_task,
                (*skills_registry).clone(),
                project_root_for_side,
                agent.identity().clone(),
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
                RoundEvent::ContextTokens(neenee_core::ContextTokenSnapshot {
                    tokens: side_context,
                    source: neenee_core::ContextTokenSource::Projection,
                }),
            ));
            *side.write().await = Some(side_session);
            active_view_side.store(true, Ordering::SeqCst);
            // Tell the TUI to enter the side view (seeds the
            // side buffer + records the routing keys) before
            // the first side round starts streaming.
            let _ = resp_tx.send(AgentResponse::SideViewOpened {
                side_id: side_id.clone(),
                primary_id,
            });
            record_invocation(session, name, args).await;
            // Stream coarse primary-status updates to the
            // side banner while the side is live. Emits an
            // immediate baseline so the banner is never
            // empty, then self-terminates on side teardown.
            spawn_parent_status_watcher(side.clone(), lifecycle.clone(), resp_tx.clone());
            if !prompt.is_empty() {
                start_active_turn(
                    active_view_side,
                    side,
                    agent,
                    session,
                    lifecycle,
                    resp_tx,
                    config,
                    RoundInput {
                        prompt: prompt.to_string(),
                        hidden: false,
                        display_prompt: None,
                        sent_at_ms: None,
                        images: Vec::new(),
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
            let _ = resp_tx.send(round_response(
                &session.id().await,
                RoundEvent::Activity("compacting context".to_string()),
            ));
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
                         • a cron: `*/5 * * * *`, `0 9 * * 1-5`\n\
                         • a countdown: `10m`, `2h30m`, `in 2 hours 30 minutes`\n\
                         • an absolute time: `14:00`, `tomorrow 09:00`, `2026-03-15 14:00`\n\
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
                        let _ = req_tx_for_commands.send(AgentRequest::Chat {
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
            match init_neenee_config(std::path::Path::new(target)) {
                Ok(created) if created.is_empty() => {
                    record_command(
                        session,
                        resp_tx,
                        name,
                        args,
                        CommandResult::Text(format!(
                            "neenee is already configured in '{}'. Nothing to do.",
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
                            "Initialized neenee configuration in '{}'.\nCreated:\n{}",
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
        Some(BuiltinCmd::Reload) => {
            // ADR-0085 §6: re-read config.toml and apply the diff live, so
            // editing the MCP servers / permissions / bash policy / hooks no
            // longer requires a restart. Reload is user-triggered (not
            // fs-watch): only the user knows when their edit is complete, and
            // a half-written file would otherwise tear down live sessions.
            let mut reloaded = Config::load();
            // Re-apply the project-scope MCP AND hooks layer (ADR-0085 §2/§3 +
            // hooks extension): a project `.neenee/config.toml` edit is exactly
            // the kind of change `/reload` exists to surface without a restart.
            // Both are still gated by trust (§5): untrusted projects load
            // nothing here.
            let project_trusted = trust_gate.is_trusted(project_root_for_side);
            let project_mcp = Config::load_project_mcp(project_root_for_side);
            if project_trusted && !project_mcp.is_empty() {
                reloaded.merge_project_mcp(project_mcp);
            }
            let project_hooks = Config::load_project_hooks(project_root_for_side);
            if project_trusted && !project_hooks.is_empty() {
                reloaded.merge_project_hooks(project_hooks);
            }
            // MCP: diff + (re)connect/disconnect. The next request picks up the
            // new tool set automatically (visible_tools recomputes each turn).
            let report = mcp_runtime.reconfigure(reloaded.mcp.clone()).await;

            // Re-apply the agent-scoped config sections that are otherwise
            // seeded only at startup. Each setter is replace-style and safe to
            // re-run; permissions seeding is additive (new allow-rules take
            // effect; removed rules are noted but not revoked this session).
            // Bash policy: harden for untrusted projects (P2). Mirrors the
            // bootstrap decision: a config edit must not drop the untrusted
            // `confirm` rule mid-run.
            let effective_bash_policy = if project_trusted {
                reloaded.bash_policy.clone()
            } else {
                reloaded.bash_policy.clone().with_untrusted_hardening()
            };
            agent.set_bash_policy(&effective_bash_policy);
            agent.set_hard_stop_turns(reloaded.principal.hard_stop_turns);
            agent.set_doom_guard_config(reloaded.principal.nudge.clone());
            agent.set_allow_model_stdin(reloaded.principal.allow_model_stdin);
            agent.set_skip_interactive_input(reloaded.principal.skip_interactive_input);
            agent.set_hooks(crate::hooks::build_hook_registry(&reloaded.hooks));
            agent.seed_permissions_from_config(&reloaded.permissions.allow);
            crate::agent_setup::reseed_prune_threshold(agent, &reloaded);
            crate::agent_setup::reseed_tool_variants(agent, &reloaded);

            let mut lines = Vec::new();
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
            lines.push("Re-applied bash policy, hooks, principal, and permissions.".to_string());
            record_command(
                session,
                resp_tx,
                name,
                args,
                CommandResult::Text(format!("Config reloaded.\n{}", lines.join("\n"))),
            )
            .await;
            send_harness_state(resp_tx, &session.id().await, agent, LoopStatus::Idle);
        }
        Some(BuiltinCmd::Trust) => {
            // ADR-0085 §5 (+ hooks extension): grant trust for this project,
            // then activate its project-scope MCP servers AND hooks by
            // reconfiguring/re-seeding with the merged config. Trust applies to
            // the git repo root, so subdirectories and worktrees share one
            // grant.
            let project_mcp = Config::load_project_mcp(project_root_for_side);
            let project_hooks = Config::load_project_hooks(project_root_for_side);
            if project_mcp.is_empty() && project_hooks.is_empty() {
                record_command(
                    session,
                    resp_tx,
                    name,
                    args,
                    CommandResult::Text(
                        "No MCP servers or hooks declared in .neenee/config.toml. Nothing to trust."
                            .to_string(),
                    ),
                )
                .await;
            } else {
                let newly = trust_gate.trust(project_root_for_side);
                // Build the effective config = global + now-trusted project MCP
                // and hooks.
                let mut reloaded = Config::load();
                reloaded.merge_project_mcp(project_mcp);
                reloaded.merge_project_hooks(project_hooks);
                let report = mcp_runtime.reconfigure(reloaded.mcp.clone()).await;
                // Re-seed the hook registry so newly-trusted project hooks take
                // effect immediately (same path as `/reload`).
                agent.set_hooks(crate::hooks::build_hook_registry(&reloaded.hooks));
                // Trust granted: re-seed bash policy with the RAW (un-hardened)
                // config so the untrusted `confirm` rule is dropped now that the
                // project is trusted.
                agent.set_bash_policy(&reloaded.bash_policy);
                let connected: Vec<&str> = report
                    .connected
                    .iter()
                    .filter(|(_, ok)| *ok)
                    .map(|(n, _)| n.as_str())
                    .collect();
                let msg = if newly {
                    let mcp_part = if connected.is_empty() {
                        String::new()
                    } else {
                        format!(" MCP activated: {}.", connected.join(", "))
                    };
                    format!("Project trusted.{mcp_part} Project hooks loaded.")
                } else {
                    "Project already trusted; reloaded its MCP servers and hooks.".to_string()
                };
                record_command(session, resp_tx, name, args, CommandResult::Text(msg)).await;
            }
            send_harness_state(resp_tx, &session.id().await, agent, LoopStatus::Idle);
        }
        Some(BuiltinCmd::Untrust) => {
            // ADR-0085 §5 (+ hooks extension): revoke trust and disconnect
            // project-scope MCP by reconfiguring with global-only config
            // (project servers vanish from the set → reconfigure removes them).
            // Project hooks are also dropped: re-seeding with global-only hooks
            // leaves the registry free of any project-supplied commands.
            let was_trusted = trust_gate.untrust(project_root_for_side);
            let reloaded = Config::load(); // global only; no project merge now
            let report = mcp_runtime.reconfigure(reloaded.mcp.clone()).await;
            agent.set_hooks(crate::hooks::build_hook_registry(&reloaded.hooks));
            // Trust revoked: re-seed the hardened bash policy so the
            // untrusted `confirm` rule is back in force for fetch/install/
            // pipe-to-shell commands.
            let hardened = reloaded.bash_policy.clone().with_untrusted_hardening();
            agent.set_bash_policy(&hardened);
            let msg = if was_trusted {
                let mcp_part = if report.removed.is_empty() {
                    String::new()
                } else {
                    format!(": {}", report.removed.join(", "))
                };
                format!(
                    "Project untrusted. Disconnected project MCP servers{mcp_part}. Project hooks unloaded."
                )
            } else {
                "Project was not trusted; nothing to revoke.".to_string()
            };
            record_command(session, resp_tx, name, args, CommandResult::Text(msg)).await;
            send_harness_state(resp_tx, &session.id().await, agent, LoopStatus::Idle);
        }
        Some(BuiltinCmd::Skills) => {
            let sub = parts.get(1).copied().unwrap_or("list");
            match sub {
                "list" => {
                    let tool = ListSkillsTool {
                        registry: skills_registry_for_commands.clone(),
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
                    registry: skills_registry_for_commands.clone(),
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
        Some(BuiltinCmd::Clear) => {
            let _ = session.replace_messages(Vec::new()).await;
            agent.clear_todos();
            let _ = session.set_todos(neenee_core::TodoList::default()).await;
            let _ = resp_tx.send(AgentResponse::ConversationCleared);
            let _ = resp_tx.send(round_response(
                &session.id().await,
                RoundEvent::TodosUpdated(neenee_core::TodoList::default()),
            ));
            record_command(
                session,
                resp_tx,
                name,
                args,
                CommandResult::Text("Conversation history cleared.".to_string()),
            )
            .await;
            // `/clear` removes transcript content but deliberately preserves
            // the session's monotonic round counter. Re-publish it after the
            // generic ConversationCleared reset so the frontend does not
            // mistake clearing history for starting a new session.
            send_harness_state(resp_tx, &session.id().await, agent, LoopStatus::Idle);
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
                                             If your terminal did not capture it, run neenee in a \
                                             clipboard-capable environment.",
                            messages.len(),
                            char_count
                        )),
                    )
                    .await;
                }
                Err(error) => {
                    record_error(
                        session,
                        resp_tx,
                        name,
                        args,
                        format!(
                            "Export built ({} chars) but clipboard copy failed: {}",
                            char_count, error
                        ),
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
                        neenee_persistence::paths::get().project_network_dir(project_root_for_side);
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
                    // this now". Out-of-band fields (nested envoy children,
                    // envoy_meta, attribution, origin, hidden) are stripped via
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
                        snapshot.push(Message::new(neenee_core::Role::User, "This is a test."));
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
                    let estimated_bytes = estimate_bytes(&messages);
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
                        neenee_persistence::paths::get().project_debug_dir(project_root_for_side);
                    let stamp = timestamp.format("%Y%m%d-%H%M%S%.3f");
                    let file = dir.join(format!("{stamp}_preview.json"));
                    let record = serde_json::json!({
                        "timestamp": timestamp.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                        "session_id": session_id,
                        "provider": provider_id,
                        "model": model_name,
                        "context_window_tokens": window,
                        "estimated_tokens": tokens,
                        "estimated_bytes": estimated_bytes,
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
                                neenee_persistence::fsutil::atomic_write_bytes(&file, &bytes)
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
        Some(BuiltinCmd::Help) => {
            let custom_help = if commands_for_task.is_empty() {
                String::new()
            } else {
                let mut commands = commands_for_task.values().collect::<Vec<_>>();
                commands.sort_by(|left, right| left.name.cmp(&right.name));
                format!(
                    "\n\nProject commands:\n{}",
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
                    active_view_side,
                    base_tools: base_tools_for_side,
                    provider_holder: provider_for_task,
                    provider_usage,
                    skills_registry: skills_registry_for_commands,
                    commands: commands_for_task,
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
            let Some(command) = commands_for_task.get(command_name) else {
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
                active_view_side,
                side,
                agent,
                session,
                lifecycle,
                resp_tx,
                config,
                RoundInput {
                    prompt: expand_command(command, arguments),
                    hidden: false,
                    display_prompt: Some(cmd),
                    sent_at_ms: None,
                    images: Vec::new(),
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
            CommandResult::Text("No scheduled jobs.".to_string()),
        )
        .await;
        return;
    }
    jobs.sort_by_key(|j| j.next_fire);
    let mut lines = vec!["Scheduled jobs:".to_string()];
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
        CommandResult::Text(lines.join("\n")),
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
            let _ = req_tx.send(AgentRequest::Chat {
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

/// Parse the argument of `/autopilot` (already lowercased by the caller).
///
/// - `""` (bare `/autopilot`, no argument) → `Ok(None)`: the dispatch flips
///   the current state, so the command doubles as a toggle.
/// - `on` / `true` / `1` → `Ok(Some(true))`
/// - `off` / `false` / `0` → `Ok(Some(false))`
/// - anything else → `Err` with a usage hint.
fn parse_autopilot_arg(arg: &str) -> Result<Option<bool>, String> {
    match arg {
        "" => Ok(None),
        "on" | "true" | "1" => Ok(Some(true)),
        "off" | "false" | "0" => Ok(Some(false)),
        other => Err(format!(
            "Unknown value '{other}'. Use `/autopilot` to toggle, or `/autopilot on|off`."
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

    /// convenience wrapper so the tests read like the public parse path
    fn split_schedule_arg(rest: &str) -> (String, String) {
        split_schedule_spec(rest).unwrap_or(("".into(), "".into()))
    }
}

#[cfg(test)]
mod autopilot_arg_tests {
    use super::parse_autopilot_arg;

    #[test]
    fn bare_argument_means_toggle() {
        // A bare `/autopilot` (no argument) yields `None` so the dispatch
        // flips the current state.
        assert_eq!(parse_autopilot_arg(""), Ok(None));
    }

    #[test]
    fn on_forms_enable() {
        assert_eq!(parse_autopilot_arg("on"), Ok(Some(true)));
        assert_eq!(parse_autopilot_arg("true"), Ok(Some(true)));
        assert_eq!(parse_autopilot_arg("1"), Ok(Some(true)));
    }

    #[test]
    fn off_forms_disable() {
        assert_eq!(parse_autopilot_arg("off"), Ok(Some(false)));
        assert_eq!(parse_autopilot_arg("false"), Ok(Some(false)));
        assert_eq!(parse_autopilot_arg("0"), Ok(Some(false)));
    }

    #[test]
    fn unknown_value_is_an_error_with_a_usage_hint() {
        let err = parse_autopilot_arg("maybe").unwrap_err();
        assert!(
            err.contains("`/autopilot` to toggle") && err.contains("`/autopilot on|off`"),
            "usage hint missing the toggle form: {err}"
        );
    }
}
