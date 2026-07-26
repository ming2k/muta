//! The `AgentRequest::SlashCommand` dispatcher, extracted verbatim from the
//! agent background task's `match req { … }` dispatch.
//!
//! This is the largest handler — it fans the parsed command out across every
//! `BuiltinCmd` variant (`/models`, `/mcp`, `/compact`, `/clear`,
//! `/permissions`, `/unattended`, `/review`, `/search`, `/resume`,
//! `/session`, `/sessions`, `/btw`, `/pursue`, `/repeat`, `/init`,
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
use neenee_agent::orchestration::{
    ContextProjectionSettings, PursuitContext, RoundInput, compact_round_history,
    emit_pursuit_updated, refresh_agent_pursuit, restore_agent_pursuit, round_response,
    send_compaction, send_harness_state, start_pursuit, stop_superseded_pursuit,
};
use neenee_core::{
    AgentNotice, AgentRequest, AgentResponse, CronExpr, LoopStatus, Message, NoticeKind,
    NoticeSeverity, NoticeSource, NoticeSurface, Provider, Pursuit, RoundEvent, Tool,
    estimate_bytes, estimate_tokens,
};
use neenee_persistence::{
    RepeatStore, config::Config, embedding, provider_usage::ProviderUsage, session::SessionStore,
};
use neenee_skills::{ListSkillsTool, SkillRegistry, UseSkillTool};

use std::collections::HashMap;
use std::sync::{
    Arc, RwLock,
    atomic::{AtomicBool, Ordering},
};
use tokio::sync::{RwLock as AsyncRwLock, mpsc};

use crate::agent_setup::active_context_window;
use crate::pursuits::{format_pursuit_budget, format_pursuit_status, parse_pursuit_budget};
use crate::review::format_review_report;
use crate::session_view::{build_sessions_overview, resume_session, short_session_id};
use crate::side::{SideSession, spawn_parent_status_watcher, start_active_turn};
use crate::slash_handler::{SlashCommandRegistry, SlashContext};
use crate::startup::{BuiltinCmd, StartupMode, split_custom_command};

async fn supersede_for_session_switch(
    lifecycle: &RoundLifecycle,
    agent: &Agent,
    session: &SessionStore,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
) {
    lifecycle.supersede();
    agent.reject_pending_permissions();
    agent.reject_pending_user_questions();
    agent.reject_pending_inputs();
    let _ = resp_tx.send(AgentResponse::PermissionsCleared);
    lifecycle.cancel_current().await;
    let session_id = session.id().await;
    stop_superseded_pursuit(
        agent,
        session,
        resp_tx,
        &session_id,
        "superseded by a session switch",
    )
    .await;
}

/// `AgentRequest::SlashCommand` — parse the command, dispatch to the matching
/// built-in handler, or fall through to the user-defined project-command path.
#[allow(clippy::too_many_arguments)]
pub async fn dispatch(
    cmd: String,
    config: &Config,
    agent: &Arc<Agent>,
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
    repeat_store_for_commands: &RepeatStore,
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
    // Record the slash invocation as a durable, non-driving echo so it
    // survives resume/export/audit (ADR-0050). This happens for EVERY command
    // uniformly — the literal `/cmd` text is persisted, never sent to the
    // model (projected out during model-request assembly), and on resume is
    // reconstructed with `UserMessageOrigin::Slash`. Commands whose effects
    // mutate state or stream a reply still do so independently; the echo is
    // purely the invocation record. Best-effort: a failed persist logs but
    // does not abort dispatch, since the command's primary effect is more
    // important than the echo.
    if let Err(error) = session
        .mutate_messages(|w| w.push(Message::command_echo(&cmd)))
        .await
    {
        tracing::warn!(?error, cmd = %cmd, "could not persist command echo");
    }
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
                let _ = resp_tx.send(round_response(
                    &session.id().await,
                    RoundEvent::Text("Always-allowed tool rules cleared.".to_string()),
                ));
            } else {
                let allowed = agent.allowed_tools();
                let message = if allowed.is_empty() {
                    "No tools are always allowed for this process.".to_string()
                } else {
                    format!("Always-allowed tools:\n- {}", allowed.join("\n- "))
                };
                let _ = resp_tx.send(round_response(
                    &session.id().await,
                    RoundEvent::Text(message),
                ));
            }
        }
        Some(BuiltinCmd::Unattended) => {
            let next = match parts.get(1).map(|s| s.to_lowercase()).as_deref() {
                Some("on") | Some("true") | Some("1") => Some(true),
                Some("off") | Some("false") | Some("0") => Some(false),
                Some(other) => {
                    let _ = resp_tx.send(AgentResponse::Error(format!(
                        "Unknown value '{}'. Use `/unattended on|off`.",
                        other
                    )));
                    return;
                }
                None => None,
            };
            let enabled = next.unwrap_or_else(|| !agent.get_unattended());
            agent.set_unattended(enabled);
            let _ = resp_tx.send(round_response(
                &session.id().await,
                RoundEvent::Text(format!(
                    "Unattended {}: the agent {} run without human intervention — the question \
                     tool is reclaimed, tool permissions auto-approve, and no prompts or \
                     questions can pause the session.",
                    if enabled { "ON" } else { "OFF" },
                    if enabled { "will" } else { "won't" },
                )),
            ));
            let _ = resp_tx.send(round_response(
                &session.id().await,
                RoundEvent::UnattendedChanged(enabled),
            ));
            // No `send_harness_state` here: toggling unattended is not a
            // round lifecycle transition, so emitting a `HarnessState("idle")`
            // would make the HarnessState handler clear the live activity
            // cell (`activity_status`) and momentarily hide the activity bar
            // mid-turn. The `UnattendedChanged` event above already mirrors
            // the new value into the TUI snapshot without that side effect.
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
                let _ = resp_tx.send(AgentResponse::Error(
                    "`/review` takes no arguments. Usage: `/review` runs an \
                                     on-demand diagnostic of the current round."
                        .to_string(),
                ));
                return;
            }
            let transcript = session.full_transcript().await;
            let turns = Agent::estimate_completed_turns(&transcript);
            if turns == 0 {
                let _ = resp_tx.send(round_response(
                    &session.id().await,
                    RoundEvent::Text(
                        "Nothing to review yet — no ReAct turns in the current \
                         round."
                            .to_string(),
                    ),
                ));
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
            let _ = resp_tx.send(round_response(
                &session.id().await,
                RoundEvent::Text(format_review_report(&verdicts, turns)),
            ));
        }
        Some(BuiltinCmd::Search) => {
            let query = cmd.strip_prefix("/search").unwrap_or("").trim();
            if query.is_empty() {
                let _ = resp_tx.send(round_response(
                    &session.id().await,
                    RoundEvent::Text("Usage: /search <query>".to_string()),
                ));
            } else {
                let messages = session.full_transcript().await;
                {
                    let mut store = embedding_store_for_commands.write().await;
                    let session_id = session.id().await;
                    if let Err(error) = store.index(&messages, &session_id).await {
                        let _ = resp_tx.send(AgentResponse::Error(error));
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
                        if results.is_empty() {
                            let _ = resp_tx.send(round_response(
                                &session.id().await,
                                RoundEvent::Text("No relevant history found.".to_string()),
                            ));
                        } else {
                            let mut lines =
                                vec!["Relevant history (most similar first):".to_string()];
                            for (i, (text, score)) in results.iter().enumerate() {
                                lines.push(format!("{}. [score={:.3}]\n{}", i + 1, score, text));
                            }
                            let _ = resp_tx.send(round_response(
                                &session.id().await,
                                RoundEvent::Text(lines.join("\n\n")),
                            ));
                        }
                    }
                    Err(error) => {
                        let _ = resp_tx.send(AgentResponse::Error(error));
                    }
                }
            }
        }
        Some(BuiltinCmd::Resume) => {
            supersede_for_session_switch(lifecycle, agent, session, resp_tx).await;
            match resume_session(session, parts.get(1).copied()).await {
                Ok((id, transcript)) => {
                    restore_agent_pursuit(agent, session).await;
                    let _ = resp_tx.send(AgentResponse::ConversationReplaced(transcript));
                    let _ = resp_tx.send(round_response(
                        &session.id().await,
                        RoundEvent::Text(format!("Resumed session {}.", short_session_id(&id))),
                    ));
                    send_harness_state(resp_tx, &session.id().await, agent, LoopStatus::Idle);
                }
                Err(error) => {
                    let _ = resp_tx.send(AgentResponse::Error(error));
                }
            }
        }
        Some(BuiltinCmd::Session) => match parts.get(1).copied().unwrap_or("status") {
            "status" => {
                let id = session.id().await;
                let parent_id = session
                    .parent_id()
                    .await
                    .unwrap_or_else(|| "none".to_string());
                let message_count = session.model_window().await.len();
                let archived_count = session.archived_transcript_count().await;
                let checkpoint = session.checkpoint().await;
                let last_projection = session.last_projection().await;
                let checkpoint_text = checkpoint
                    .map(|item| {
                        format!(
                            "{} {}/{} ({})",
                            item.pursuit, item.iteration, item.max_iterations, item.status
                        )
                    })
                    .unwrap_or_else(|| "none".to_string());
                let _ = resp_tx.send(round_response(
                                        &session.id().await,
                                        RoundEvent::Text(format!(
                                    "Session: {}\nForked from: {}\nModel-window messages: {}\nArchived transcript messages: {}\nLoop checkpoint: {}\nLast context projection: {}",
                                    id,
                                    parent_id,
                                    message_count,
                                    archived_count,
                                    checkpoint_text,
                                    last_projection
                                        .map(|item| format!(
                                            "{:?}: {} -> {} chars",
                                            item.operation, item.before_chars, item.after_chars
                                        ))
                                        .unwrap_or_else(|| "none".to_string())
                                )),
                                    ));
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
                    let _ = resp_tx.send(round_response(
                        &session.id().await,
                        RoundEvent::Text(format!("Sessions:\n{}", lines.join("\n"))),
                    ));
                }
                Err(error) => {
                    let _ = resp_tx.send(AgentResponse::Error(error));
                }
            },
            "fork" => {
                supersede_for_session_switch(lifecycle, agent, session, resp_tx).await;
                match session.fork().await {
                    Ok((id, parent_id)) => {
                        restore_agent_pursuit(agent, session).await;
                        let _ = resp_tx.send(round_response(
                            &session.id().await,
                            RoundEvent::Text(format!("Forked session {} from {}.", id, parent_id)),
                        ));
                        send_harness_state(resp_tx, &session.id().await, agent, LoopStatus::Idle);
                    }
                    Err(error) => {
                        let _ = resp_tx.send(AgentResponse::Error(error));
                    }
                }
            }
            "open" => {
                let Some(id) = parts.get(2) else {
                    let _ = resp_tx.send(AgentResponse::Error(
                        "Usage: /session open <session-id>".to_string(),
                    ));
                    return;
                };
                supersede_for_session_switch(lifecycle, agent, session, resp_tx).await;
                match session.open(id).await {
                    Ok(()) => {
                        restore_agent_pursuit(agent, session).await;
                        let transcript = session.full_transcript().await;
                        let _ = resp_tx.send(AgentResponse::ConversationReplaced(transcript));
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
                        let _ = resp_tx.send(round_response(
                            &session.id().await,
                            RoundEvent::Text(format!("Opened session {}.", id)),
                        ));
                        send_harness_state(resp_tx, &session.id().await, agent, LoopStatus::Idle);
                    }
                    Err(error) => {
                        let _ = resp_tx.send(AgentResponse::Error(error));
                    }
                }
            }
            "resume" => {
                supersede_for_session_switch(lifecycle, agent, session, resp_tx).await;
                match resume_session(session, parts.get(2).copied()).await {
                    Ok((id, transcript)) => {
                        restore_agent_pursuit(agent, session).await;
                        let _ = resp_tx.send(AgentResponse::ConversationReplaced(transcript));
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
                        let _ = resp_tx.send(round_response(
                            &session.id().await,
                            RoundEvent::Text(format!("Resumed session {}.", short_session_id(&id))),
                        ));
                        send_harness_state(resp_tx, &session.id().await, agent, LoopStatus::Idle);
                    }
                    Err(error) => {
                        let _ = resp_tx.send(AgentResponse::Error(error));
                    }
                }
            }
            "new" => {
                supersede_for_session_switch(lifecycle, agent, session, resp_tx).await;
                agent.clear_todos();
                match session.reset().await {
                    Ok(id) => {
                        restore_agent_pursuit(agent, session).await;
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
                        let _ = resp_tx.send(round_response(
                            &session.id().await,
                            RoundEvent::Text(format!("Started new session: {}", id)),
                        ));
                    }
                    Err(error) => {
                        let _ = resp_tx.send(AgentResponse::Error(error));
                    }
                }
            }
            other => {
                let _ = resp_tx.send(AgentResponse::Error(format!(
                    "Unknown session command '{}'. Use status, list, resume, fork, open, or new.",
                    other
                )));
            }
        },
        Some(BuiltinCmd::Sessions) => {
            let _ = resp_tx.send(AgentResponse::SessionsOverview(
                build_sessions_overview(session).await,
            ));
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
                let _ = resp_tx.send(AgentResponse::Error(
                    "A side conversation is already open. \
                                     Leave it with Esc first."
                        .to_string(),
                ));
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
                    let _ = resp_tx.send(AgentResponse::Error(error));
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
                    send_compaction(resp_tx, &session.id().await, &checkpoint);
                }
                Ok(None) => {
                    let _ = resp_tx.send(round_response(
                        &session.id().await,
                        RoundEvent::Text("Not enough complete rounds to compact.".to_string()),
                    ));
                }
                Err(error) => {
                    let _ = resp_tx.send(AgentResponse::Error(error));
                }
            }
            agent.fire_post_compact().await;
        }
        Some(BuiltinCmd::Pursue) => {
            let thread_id = session.id().await;
            let argument = cmd.strip_prefix("/pursue").unwrap_or("").trim();
            let rest = argument;

            async fn report_pursuit_result(
                tx: &mpsc::UnboundedSender<AgentResponse>,
                session_id: &str,
                agent: &Agent,
                result: Result<Option<Pursuit>, String>,
                success: impl FnOnce(&Pursuit) -> String,
                empty: impl Into<String>,
            ) {
                match result {
                    Ok(Some(pursuit)) => {
                        agent.set_pursuit(pursuit.clone());
                        emit_pursuit_updated(tx, session_id, &pursuit);
                        let _ = tx.send(round_response(
                            session_id,
                            RoundEvent::Text(success(&pursuit)),
                        ));
                    }
                    Ok(None) => {
                        let _ = tx.send(AgentResponse::Error(empty.into()));
                    }
                    Err(error) => {
                        let _ = tx.send(AgentResponse::Error(error));
                    }
                }
            }

            if rest == "stop" {
                let stopped = agent.is_pursuit_armed() && lifecycle.cancel_current().await;
                if stopped {
                    if let Some(pursuit) = agent.stop_pursuit("stopped by user") {
                        match session.set_pursuit(Some(pursuit.clone())).await {
                            Ok(()) => emit_pursuit_updated(resp_tx, &session.id().await, &pursuit),
                            Err(error) => {
                                let _ = resp_tx.send(AgentResponse::Error(error));
                            }
                        }
                    }
                    let _ = resp_tx.send(round_response(
                        &thread_id,
                        RoundEvent::Text("Pursuit stop requested.".to_string()),
                    ));
                } else {
                    let _ = resp_tx.send(round_response(
                        &thread_id,
                        RoundEvent::Text("No pursuit is running.".to_string()),
                    ));
                }
                if stopped {
                    // Genuine lifecycle transition (mirrors `interrupt`):
                    // flip the harness to idle eagerly so the activity bar
                    // reflects the stopped work before terminal persistence.
                    send_harness_state(resp_tx, &session.id().await, agent, LoopStatus::Idle);
                }
                return;
            }

            if rest == "status" {
                refresh_agent_pursuit(agent, session).await;

                // SessionStart hooks (ADR-0025): inject setup context before the first
                // round. Resume vs fresh start is surfaced so a hook can branch.
                {
                    let source = match &startup {
                        StartupMode::Resume(_) => neenee_core::SessionSource::Resume,
                        _ => neenee_core::SessionSource::Startup,
                    };
                    let mut messages = session.model_window().await;
                    agent.fire_session_start(source, &mut messages).await;
                    // Persist the hook-injected setup context through the
                    // single write path so the session stays the source of
                    // truth (ADR-0048). A full replace is correct here: this
                    // is a rare startup path, not the per-round hot loop.
                    if let Err(err) = session.replace_messages(messages).await {
                        let _ = resp_tx.send(AgentResponse::Error(err));
                    }
                }
                let armed = agent.is_pursuit_armed();
                let iterations = agent.pursuit_iterations();
                let message = match agent.get_pursuit() {
                    Some(pursuit) => {
                        let mut m = format_pursuit_status(&pursuit);
                        if armed {
                            let stats = agent.pursuit_stats();
                            let pass = iterations
                                .saturating_add(1)
                                .min(neenee_agent::MAX_PURSUIT_ITERATIONS);
                            m.push_str(&format!(
                                "\nPursuit active · pass {pass}/{} · \
                                 {} completed pass{}, {} tokens, {:.0}s",
                                neenee_agent::MAX_PURSUIT_ITERATIONS,
                                stats.passes,
                                if stats.passes == 1 { "" } else { "es" },
                                stats.tokens,
                                stats.wall_clock_ms as f64 / 1000.0
                            ));
                        }
                        m
                    }
                    None => "No active pursuit. Start one with /pursue <condition>.".to_string(),
                };
                let _ = resp_tx.send(round_response(&thread_id, RoundEvent::Text(message)));
            } else if rest == "clear" {
                agent.disarm_pursuit();
                match session.set_pursuit(None).await {
                    Ok(_) => {
                        if agent.get_pursuit().is_some() {
                            agent.clear_pursuit();
                            // Mirror the cleared pursuit into the TUI snapshot
                            // via the non-gated channel so the activity bar's
                            // `⟴` badge updates without flushing the live
                            // activity cell (which a `HarnessState("idle")`
                            // would do, flickering the bar mid-round).
                            let _ = resp_tx
                                .send(round_response(&thread_id, RoundEvent::PursuitCleared));
                            let _ = resp_tx.send(round_response(
                                &thread_id,
                                RoundEvent::Text("Pursuit cleared.".to_string()),
                            ));
                        } else {
                            let _ = resp_tx.send(round_response(
                                &thread_id,
                                RoundEvent::Text("No pursuit to clear.".to_string()),
                            ));
                        }
                    }
                    Err(error) => {
                        let _ = resp_tx.send(AgentResponse::Error(error));
                    }
                }
            } else if rest == "done" {
                agent.disarm_pursuit();
                report_pursuit_result(
                    resp_tx,
                    &thread_id,
                    agent,
                    session.mark_pursuit_complete().await,
                    |_| "Pursuit marked completed.".to_string(),
                    "No pursuit to complete.",
                )
                .await;
            } else if rest.starts_with("edit ") {
                let new_objective = rest.strip_prefix("edit ").unwrap_or("").trim();
                if new_objective.is_empty() {
                    let _ = resp_tx.send(AgentResponse::Error(
                        "Usage: /pursue edit <new condition>".to_string(),
                    ));
                } else {
                    match session.update_pursuit_objective(new_objective).await {
                        Ok(Some(pursuit)) => {
                            agent.set_pursuit(pursuit.clone());
                            {
                                let mut messages = session.model_window().await;
                                agent.inject_objective_updated(&mut messages);
                                let _ = session.replace_messages(messages).await;
                            }
                            emit_pursuit_updated(resp_tx, &thread_id, &pursuit);
                            let _ = resp_tx.send(round_response(
                                &thread_id,
                                RoundEvent::Text(format!("Pursuit updated: {}", pursuit.objective)),
                            ));
                        }
                        Ok(None) => {
                            let _ = resp_tx.send(AgentResponse::Error(
                                "No pursuit to edit. Start one with /pursue <condition>."
                                    .to_string(),
                            ));
                        }
                        Err(error) => {
                            let _ = resp_tx.send(AgentResponse::Error(error));
                        }
                    }
                }
            } else if rest == "pause" || rest == "resume" {
                let _ = resp_tx.send(AgentResponse::Error(
                    "/pursue pause and /pursue resume are not supported. Use /pursue \
                     <condition>, /pursue edit, /pursue done, /pursue clear, /pursue status, \
                     /pursue budget, or /pursue stop."
                        .to_string(),
                ));
            } else if rest == "budget" || rest.starts_with("budget ") {
                // `/pursue budget passes=20 tokens=500000 time=1800000` sets hard
                // budgets on the active pursuit (ADR-0069). Any subset may be
                // given; an axis omitted leaves it uncapped. `/pursue budget`
                // (no args) clears the budget. Budgets are opt-in only and never
                // invented by the model.
                let args = rest.strip_prefix("budget").unwrap_or("").trim();
                match session.pursuit().await {
                    Some(mut pursuit) if !pursuit.is_complete => match parse_pursuit_budget(args) {
                        Ok(budget) => {
                            pursuit.budget = budget;
                            match session.set_pursuit(Some(pursuit.clone())).await {
                                Ok(_) => {
                                    agent.set_pursuit(pursuit.clone());
                                    emit_pursuit_updated(resp_tx, &thread_id, &pursuit);
                                    let label = format_pursuit_budget(pursuit.budget);
                                    let _ = resp_tx.send(round_response(
                                        &thread_id,
                                        RoundEvent::Text(format!("Pursuit budget {label}.")),
                                    ));
                                }
                                Err(error) => {
                                    let _ = resp_tx.send(AgentResponse::Error(error));
                                }
                            }
                        }
                        Err(error) => {
                            let _ = resp_tx.send(AgentResponse::Error(error));
                        }
                    },
                    Some(_) => {
                        let _ = resp_tx.send(AgentResponse::Error(
                            "Cannot set a budget on a completed pursuit.".to_string(),
                        ));
                    }
                    None => {
                        let _ = resp_tx.send(AgentResponse::Error(
                            "No pursuit to budget. Start one with /pursue <condition>.".to_string(),
                        ));
                    }
                }
            } else {
                // `/pursue <condition>` sets a fresh condition and drives it;
                // `/pursue` (empty) re-arms and drives the existing pursuit.
                let resume_runtime = rest.is_empty() && agent.is_pursuit_armed();
                let condition = if rest.is_empty() {
                    match session.pursuit().await {
                        Some(pursuit) if !pursuit.is_complete => {
                            let _ = resp_tx.send(round_response(
                                &thread_id,
                                RoundEvent::Text(format!(
                                    "Resuming pursuit on existing pursuit: {}",
                                    pursuit.objective
                                )),
                            ));
                            pursuit.objective
                        }
                        _ => {
                            let _ = resp_tx.send(AgentResponse::Error(
                                "No active pursuit. Start one with /pursue <condition>."
                                    .to_string(),
                            ));
                            return;
                        }
                    }
                } else {
                    let pursuit = Pursuit {
                        objective: rest.to_string(),
                        is_complete: false,
                        ..Default::default()
                    };
                    match session.set_pursuit(Some(pursuit.clone())).await {
                        Ok(_) => {
                            agent.set_pursuit(pursuit.clone());
                            emit_pursuit_updated(resp_tx, &thread_id, &pursuit);
                            pursuit.objective
                        }
                        Err(error) => {
                            let _ = resp_tx.send(AgentResponse::Error(error));
                            return;
                        }
                    }
                };
                start_pursuit(
                    PursuitContext {
                        agent: agent.clone(),
                        tx: resp_tx.clone(),
                        lifecycle: lifecycle.clone(),
                        session: session.clone(),
                        session_id: session.id().await,
                        projection: ContextProjectionSettings::from_config(
                            config,
                            active_context_window(agent),
                        ),
                        retry_max_attempts: config.provider_retry_max_attempts,
                        retry_base_ms: config.provider_retry_base_ms,
                        retry_max_ms: config.provider_retry_max_ms,
                        resume_runtime,
                    },
                    condition,
                )
                .await;
            }
            // `/pursue status` / unsupported-subcommand paths reach here. None
            // of them mutate harness state, so there is nothing to mirror and
            // no round boundary to signal — a `HarnessState("idle")` here would
            // only flicker the activity bar.
        }
        Some(BuiltinCmd::Repeat) => {
            let rest = cmd.strip_prefix("/repeat").unwrap_or("").trim();
            if rest.is_empty() || rest == "help" {
                let _ = resp_tx.send(round_response(
                                    &session.id().await,
                                    RoundEvent::Text(
                                        "Usage: /repeat <cron> <prompt>\n\
                                         cron is five fields: minute hour day month weekday \
                                         (e.g. `*/5 * * * *` = every 5 min, `0 9 * * 1-5` = 09:00 weekdays).\n\
                                         Also: /repeat list, /repeat cancel <id>."
                                            .to_string(),
                                    ),
                                ));
                return;
            }
            if rest == "list" {
                let jobs = repeat_store_for_commands.list().await.unwrap_or_default();
                if jobs.is_empty() {
                    let _ = resp_tx.send(round_response(
                        &session.id().await,
                        RoundEvent::Text("No /repeat jobs scheduled.".to_string()),
                    ));
                } else {
                    let mut lines = vec!["Scheduled /repeat jobs:".to_string()];
                    for j in &jobs {
                        lines.push(format!(
                            "  {} · `{}` · next {} · {}",
                            &j.id[..8.min(j.id.len())],
                            j.cron,
                            j.next_fire.format("%Y-%m-%d %H:%M"),
                            j.prompt,
                        ));
                    }
                    let _ = resp_tx.send(round_response(
                        &session.id().await,
                        RoundEvent::Text(lines.join("\n")),
                    ));
                }
                return;
            }
            if let Some(id) = rest.strip_prefix("cancel ") {
                let id = id.trim();
                match repeat_store_for_commands.delete(id).await {
                    Ok(true) => {
                        let _ = resp_tx.send(round_response(
                            &session.id().await,
                            RoundEvent::Text(format!("Cancelled repeat job {id}.")),
                        ));
                    }
                    Ok(false) => {
                        let _ = resp_tx.send(round_response(
                            &session.id().await,
                            RoundEvent::Text(format!("No repeat job with id {id}.")),
                        ));
                    }
                    Err(error) => {
                        let _ = resp_tx.send(AgentResponse::Error(error));
                    }
                }
                return;
            }
            // `/repeat <5-field cron> <prompt>`
            let tokens: Vec<&str> = rest.split_whitespace().collect();
            if tokens.len() < 6 {
                let _ = resp_tx.send(AgentResponse::Error(
                    "Usage: /repeat <5-field cron> <prompt>. \
                                      Example: /repeat */5 * * * * check the deploy"
                        .to_string(),
                ));
                return;
            }
            let cron = tokens[0..5].join(" ");
            let prompt = tokens[5..].join(" ");
            let parsed = match CronExpr::parse(&cron) {
                Ok(p) => p,
                Err(error) => {
                    let _ = resp_tx.send(AgentResponse::Error(format!("Invalid cron: {error}")));
                    return;
                }
            };
            let now = chrono::Utc::now();
            let next = match parsed.next_fire(now) {
                Some(n) => n,
                None => {
                    let _ = resp_tx.send(AgentResponse::Error(
                        "That cron expression never fires within the next year.".to_string(),
                    ));
                    return;
                }
            };
            match repeat_store_for_commands.add(&cron, &prompt, next).await {
                Ok(job) => {
                    let _ = resp_tx.send(round_response(
                        &session.id().await,
                        RoundEvent::Text(format!(
                            "Scheduled repeat job {} (`{}`), next {}. Running now.",
                            &job.id[..8.min(job.id.len())],
                            cron,
                            next.format("%Y-%m-%d %H:%M"),
                        )),
                    ));
                    // Fire the first run immediately (cron handles the rest).
                    let _ = req_tx_for_commands.send(AgentRequest::Chat {
                        text: prompt,
                        images: Vec::new(),
                        sent_at_ms: None,
                    });
                }
                Err(error) => {
                    let _ = resp_tx.send(AgentResponse::Error(error));
                }
            }
        }
        Some(BuiltinCmd::Init) => {
            let target = parts.get(1).copied().unwrap_or(".");
            match init_neenee_config(std::path::Path::new(target)) {
                Ok(created) if created.is_empty() => {
                    let _ = resp_tx.send(round_response(
                        &session.id().await,
                        RoundEvent::Text(format!(
                            "neenee is already configured in '{}'. Nothing to do.",
                            target
                        )),
                    ));
                }
                Ok(created) => {
                    let _ = resp_tx.send(round_response(
                        &session.id().await,
                        RoundEvent::Text(format!(
                            "Initialized neenee configuration in '{}'.\nCreated:\n{}",
                            target,
                            created
                                .iter()
                                .map(|path| format!("- {}", path))
                                .collect::<Vec<_>>()
                                .join("\n")
                        )),
                    ));
                }
                Err(error) => {
                    let _ = resp_tx.send(AgentResponse::Error(error));
                }
            }
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
                            let _ = resp_tx.send(round_response(
                                &session.id().await,
                                RoundEvent::Text(output),
                            ));
                        }
                        Err(error) => {
                            let _ = resp_tx.send(AgentResponse::Error(error));
                        }
                    }
                }
                "reload" => {
                    skills_registry_for_commands.reload().await;
                    let count = skills_registry_for_commands.lock().list().len();
                    let _ = resp_tx.send(round_response(
                        &session.id().await,
                        RoundEvent::Text(format!("Skills reloaded. {} skill(s) available.", count)),
                    ));
                }
                other => {
                    let _ = resp_tx.send(AgentResponse::Error(format!(
                        "Unknown skills command '{}'. Use 'list' or 'reload'.",
                        other
                    )));
                }
            }
        }
        Some(BuiltinCmd::Skill) => {
            let name = cmd.strip_prefix("/skill").unwrap_or("").trim();
            if name.is_empty() {
                let _ = resp_tx.send(AgentResponse::Error("Usage: /skill <name>".to_string()));
            } else {
                let args = serde_json::json!({ "name": name }).to_string();
                let tool = UseSkillTool {
                    registry: skills_registry_for_commands.clone(),
                };
                match tool.call(&args).await {
                    Ok(output) => {
                        let _ = resp_tx.send(round_response(
                            &session.id().await,
                            RoundEvent::Text(output),
                        ));
                    }
                    Err(error) => {
                        let _ = resp_tx.send(AgentResponse::Error(error));
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
            let _ = resp_tx.send(round_response(
                &session.id().await,
                RoundEvent::Text("Conversation history cleared.".to_string()),
            ));
            // `/clear` removes transcript content but deliberately preserves
            // the session's monotonic round counter. Re-publish it after the
            // generic ConversationCleared reset so the frontend does not
            // mistake clearing history for starting a new session.
            send_harness_state(resp_tx, &session.id().await, agent, LoopStatus::Idle);
        }
        Some(BuiltinCmd::Export) => {
            let messages = session.model_window().await;
            let session_id = session.id().await;
            let provider_id = agent.provider.provider_id();
            let model_name = agent.provider.model();
            let pursuit = agent.get_pursuit();
            let markdown = crate::export::format_export_markdown(
                crate::export::ExportContext {
                    session_id: &session_id,
                    provider: &provider_id,
                    model: &model_name,
                    pursuit: pursuit.as_ref(),
                },
                &messages,
            );
            let char_count = markdown.chars().count();
            match ui.copy_to_clipboard(&markdown).await {
                Ok(crate::CopyOutcome::Native) => {
                    let _ = resp_tx.send(round_response(
                        &session.id().await,
                        RoundEvent::Text(format!(
                            "Session exported to clipboard ({} messages, {} chars). \
                                             Paste it into another agent to continue this work.",
                            messages.len(),
                            char_count
                        )),
                    ));
                }
                Ok(crate::CopyOutcome::Osc52) => {
                    let _ = resp_tx.send(round_response(
                        &session.id().await,
                        RoundEvent::Text(format!(
                            "Session exported via OSC52 ({} messages, {} chars). \
                                             If your terminal did not capture it, run neenee in a \
                                             clipboard-capable environment.",
                            messages.len(),
                            char_count
                        )),
                    ));
                }
                Err(error) => {
                    let _ = resp_tx.send(AgentResponse::Error(format!(
                        "Export built ({} chars) but clipboard copy failed: {}",
                        char_count, error
                    )));
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
                            let _ = resp_tx.send(AgentResponse::Error(format!(
                                "Unknown value '{other}'. Use `/debug trace on|off`."
                            )));
                            return;
                        }
                        None => None,
                    };
                    let enabled = next.unwrap_or_else(|| !agent.provider.debug_capture_enabled());
                    let dir =
                        neenee_persistence::paths::get().project_network_dir(project_root_for_side);
                    agent.provider.set_debug_capture(enabled, dir.clone());
                    let _ = resp_tx.send(round_response(
                        &session.id().await,
                        RoundEvent::Text(format!(
                            "Trace {}: each provider round-trip {} written to\n  {}",
                            if enabled { "ON" } else { "OFF" },
                            if enabled { "is" } else { "will no longer be" },
                            dir.display(),
                        )),
                    ));
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
                    let pursuit = agent.get_pursuit();
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
                        "pursuit": pursuit,
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
                                let _ = resp_tx.send(AgentResponse::Error(format!(
                                    "Preview write failed: {error}"
                                )));
                                return;
                            }
                        }
                        Err(error) => {
                            let _ = resp_tx.send(AgentResponse::Error(format!(
                                "Preview serialize failed: {error}"
                            )));
                            return;
                        }
                    }

                    let window_str = if window > 0 {
                        format!("of {window} ({pressure_pct}%)")
                    } else {
                        "of unknown window".to_string()
                    };
                    let _ = resp_tx.send(round_response(
                        &session.id().await,
                        RoundEvent::Text(format!(
                            "Preview (dry run, wire body, probe \"This is a test.\") — \
                             {provider_id}/{model_name}: ~{tokens} tokens {window_str}, {} \
                             message(s), {n_tools} tool(s). Full JSON: {file_path}",
                            messages.len(),
                        )),
                    ));
                }
                Some(other) => {
                    let _ = resp_tx.send(AgentResponse::Error(format!(
                        "Unknown debug target '{other}'. Available: trace, preview. \
                         Usage: `/debug trace on|off` or `/debug preview`."
                    )));
                }
                None => {
                    let trace_on = agent.provider.debug_capture_enabled();
                    let _ = resp_tx.send(round_response(
                        &session.id().await,
                        RoundEvent::Text(format!(
                            "Debug status:\n- trace: {}\n\nUsage:\n\
                             - `/debug trace on|off` — trace each provider round-trip\n\
                             - `/debug preview` — dry-run the next request to disk",
                            if trace_on { "ON" } else { "OFF" },
                        )),
                    ));
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
            let _ = resp_tx.send(round_response(
                &session.id().await,
                RoundEvent::Text(format!(
                    "{}
{custom_help}{extra_help}",
                    lines.join(
                        "
"
                    )
                )),
            ));
        }
        Some(BuiltinCmd::Exit) => {
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
                    repeat_store: repeat_store_for_commands,
                    req_tx: req_tx_for_commands,
                    project_root: project_root_for_side,
                    startup,
                    ui,
                };
                if handler.handle(ctx).await {
                    return;
                }
            }
            let (name, arguments) = split_custom_command(&cmd);
            let Some(command) = commands_for_task.get(name) else {
                let _ = resp_tx.send(AgentResponse::Error(format!(
                    "Unknown command: {}",
                    parts[0]
                )));
                return;
            };
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
