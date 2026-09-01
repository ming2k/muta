//! The main `AgentRequest::SlashCommand` dispatcher.

use std::sync::Arc;

use super::SlashEnv;
use super::record::{
    record_ack, record_command, record_command_with_duration, record_error, record_invocation,
};
use super::schedule_ops::{
    SessionRoute, add_scheduled_job, cancel_scheduled_job, list_scheduled_jobs, parse_delegate_arg,
    parse_jail_arg, session_route, split_schedule_spec,
};
use super::security_ops::{
    TrustRoute, live_custom_commands, reload_trusted_assets, runtime_workspace_security,
    trust_route,
};
use super::session_ops::{
    apply_additional_roots, fork_current_session, restore_session_runtime, start_fresh_session,
    supersede_for_session_switch, teardown_sides_for_session_switch,
};
use crate::agent_setup::active_context_window;
use crate::commands::expand_command;
use crate::project::init_muta_config;
use crate::session_view::{build_sessions_overview, short_session_id};
use crate::side::{
    SideEnv, SideSession, publish_btw_list, refuse_if_no_provider, spawn_parent_status_watcher,
    start_active_turn,
};
use crate::slash_handler::SlashContext;
use crate::startup::{BuiltinCmd, split_custom_command};

use muta_agent::orchestration::{
    ContextProjectionSettings, RoundInput, compact_round_history, round_response, send_compaction,
    send_harness_state_for_session,
};
use muta_contracts::{
    AgentRequest, AgentResponse, CommandResult, CronExpr, LoopStatus, Message, RoundEvent,
    ScheduledJob, Tool, TrustDomain, estimate_tokens, repeat::parse_schedule_arg,
};
use muta_persistence::config::Config;
use muta_skills::{ListSkillsTool, UseSkillTool};

/// `AgentRequest::SlashCommand` — parse the command, dispatch to the matching
/// built-in handler, or fall through to the user-defined project-command path.
pub async fn dispatch(cmd: String, mut env: SlashEnv<'_>) {
    let SlashEnv {
        config,
        agent,
        mcp_runtime,
        workspace_security,
        shared_additional_roots,
        shared_unconfined,
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
        background_jobs,
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
    // already rewrites a *picked* alias into its canonical target
    // (`insert_text`), but a *typed* alias arrives verbatim, so dispatch is
    // still the single point where `/config` becomes `/settings`.
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
            apply_additional_roots(shared_additional_roots, &reloaded, project_root_for_side);

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
                "Web tools: provider {}, reader {}.",
                reloaded.websearch.provider, reloaded.websearch.reader,
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
            send_harness_state_for_session(
                resp_tx,
                &session.id().await,
                agent,
                session,
                LoopStatus::Idle,
            )
            .await;
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
        Some(BuiltinCmd::Jail) => {
            let arg = parts.get(1).map(|s| s.to_lowercase()).unwrap_or_default();
            let next = match parse_jail_arg(&arg) {
                Ok(next) => next,
                Err(msg) => {
                    record_error(session, resp_tx, name, args, msg).await;
                    return;
                }
            };
            // A bare `/jail` (`None`) toggles the current state.
            // Note: jail = true means confined; jail = false means unconfined.
            let currently_jailed = !shared_unconfined.is_unconfined();
            let jail_enabled = next.unwrap_or(!currently_jailed);
            shared_unconfined.set_unconfined(!jail_enabled);

            let (title, detail) = if jail_enabled {
                (
                    "Workspace Jail ON (Confined)",
                    vec![
                        "File tools are confined to workspace root and temp paths".to_string(),
                        "Escapes outside admitted roots will be blocked".to_string(),
                    ],
                )
            } else {
                (
                    "Workspace Jail OFF (Unconfined)",
                    vec![
                        "Tools may access and edit any file on the host system".to_string(),
                        "Constrained only by daemon OS user permissions".to_string(),
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
                    detail: Some(detail),
                },
            )
            .await;
            let _ = resp_tx.send(round_response(
                &session.id().await,
                RoundEvent::UnconfinedChanged(!jail_enabled),
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
                        shared_unconfined: env.shared_unconfined,
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
                        background_jobs: env.background_jobs,
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
        Some(BuiltinCmd::Jobs) => {
            let sub = parts.get(1).copied().unwrap_or("list");
            match sub {
                "kill" => {
                    if let Some(target_id) = parts.get(2) {
                        let jid = muta_contracts::JobId(target_id.to_string());
                        match background_jobs.kill_job(&jid) {
                            Ok(()) => {
                                record_ack(
                                    session,
                                    name,
                                    args,
                                    format!("Terminated background job {}.", target_id),
                                )
                                .await;
                            }
                            Err(err) => {
                                record_error(
                                    session,
                                    resp_tx,
                                    name,
                                    args,
                                    &format!("Failed to kill job {}: {}", target_id, err),
                                )
                                .await;
                            }
                        }
                    } else {
                        record_error(session, resp_tx, name, args, "Usage: /jobs kill <job_id>")
                            .await;
                    }
                }
                "logs" => {
                    if let Some(target_id) = parts.get(2) {
                        let jid = muta_contracts::JobId(target_id.to_string());
                        match background_jobs.get_logs(&jid, 50) {
                            Some(lines) => {
                                let output = if lines.is_empty() {
                                    "(no logs recorded yet)".to_string()
                                } else {
                                    lines.join("\n")
                                };
                                record_command(
                                    session,
                                    resp_tx,
                                    name,
                                    args,
                                    CommandResult::Text(format!(
                                        "Logs for {}:\n```\n{}\n```",
                                        target_id, output
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
                                    &format!("Job not found: {}", target_id),
                                )
                                .await;
                            }
                        }
                    } else {
                        record_error(session, resp_tx, name, args, "Usage: /jobs logs <job_id>")
                            .await;
                    }
                }
                _ => {
                    let jobs = background_jobs.list_jobs();
                    if jobs.is_empty() {
                        record_command(
                            session,
                            resp_tx,
                            name,
                            args,
                            CommandResult::Text("No active or recent background jobs.".to_string()),
                        )
                        .await;
                    } else {
                        let mut table = String::from(
                            "### Background Jobs\n\n| ID | Type | State | Latest Output |\n|---|---|---|---|\n",
                        );
                        for j in jobs {
                            let (kind_str, detail) = match &j.spec {
                                muta_contracts::JobSpec::Process { command, label, .. } => (
                                    label.clone().unwrap_or_else(|| "process".to_string()),
                                    command.clone(),
                                ),
                                muta_contracts::JobSpec::Runner {
                                    role, description, ..
                                } => (format!("runner ({role})"), description.clone()),
                            };
                            let status_str = match &j.state {
                                muta_contracts::JobState::Queued => "Queued".to_string(),
                                muta_contracts::JobState::Running { pid, .. } => {
                                    if let Some(p) = pid {
                                        format!("Running (PID {p})")
                                    } else {
                                        "Running".to_string()
                                    }
                                }
                                muta_contracts::JobState::Succeeded { duration_ms, .. } => {
                                    format!("✓ Passed ({}s)", duration_ms / 1000)
                                }
                                muta_contracts::JobState::Failed {
                                    duration_ms,
                                    exit_code,
                                    ..
                                } => {
                                    format!("✗ Failed (Exit {exit_code}, {}s)", duration_ms / 1000)
                                }
                                muta_contracts::JobState::Killed { duration_ms } => {
                                    format!("Killed ({}s)", duration_ms / 1000)
                                }
                                muta_contracts::JobState::TimedOut { duration_ms } => {
                                    format!("Timed Out ({}s)", duration_ms / 1000)
                                }
                            };
                            let latest = j.latest_output.as_deref().unwrap_or(detail.as_str());
                            let truncated_latest = if latest.len() > 60 {
                                format!("{}...", &latest[..57])
                            } else {
                                latest.to_string()
                            };
                            table.push_str(&format!(
                                "| `{}` | {} | {} | `{}` |\n",
                                j.id.0, kind_str, status_str, truncated_latest
                            ));
                        }
                        record_command(session, resp_tx, name, args, CommandResult::Text(table))
                            .await;
                    }
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
                         - Aggregate: {}\n\
                         Asset trust does not grant filesystem scope or runtime execution permission.",
                        snapshot.root,
                        snapshot.mcp.as_str(),
                        snapshot.skills.as_str(),
                        snapshot.hooks.as_str(),
                        snapshot.rules.as_str(),
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
                         - MCP: {}; Skills: {}; Hooks: {}; Rules: {}{}",
                        report.snapshot.root,
                        granted,
                        report.snapshot.mcp.as_str(),
                        report.snapshot.skills.as_str(),
                        report.snapshot.hooks.as_str(),
                        report.snapshot.rules.as_str(),
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
            send_harness_state_for_session(
                resp_tx,
                &session.id().await,
                agent,
                session,
                LoopStatus::Idle,
            )
            .await;
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
                "status" => {
                    let status_lines = {
                        let guard = skills_registry_for_commands.lock();
                        let list = guard.list();
                        let total = list.len();
                        let quarantined = list.iter().filter(|s| s.quarantined).count();
                        let enabled = list.iter().filter(|s| s.enabled && !s.quarantined).count();
                        let user_count = list.iter().filter(|s| s.scope == muta_skills::SkillScope::User).count();
                        let repo_count = list.iter().filter(|s| s.scope == muta_skills::SkillScope::Repo).count();
                        let extra_count = list.iter().filter(|s| s.scope == muta_skills::SkillScope::Extra).count();
                        let remote_count = list.iter().filter(|s| s.scope == muta_skills::SkillScope::Remote).count();

                        let mut lines = vec![
                            format!("Skills Status: {total} total ({enabled} enabled, {quarantined} quarantined)"),
                            format!("  • User: {user_count}"),
                            format!("  • Repo: {repo_count}"),
                            format!("  • Extra: {extra_count}"),
                            format!("  • Remote: {remote_count}"),
                        ];
                        if quarantined > 0 {
                            lines.push("\nNote: Quarantined skills require authorization. Run `/trust skills` or `/trust` to enable them.".to_string());
                        }
                        lines.join("\n")
                    };
                    record_command(
                        session,
                        resp_tx,
                        name,
                        args,
                        CommandResult::Text(status_lines),
                    )
                    .await;
                }
                "reload" => {
                    record_command(
                        session,
                        resp_tx,
                        name,
                        args,
                        CommandResult::Text(
                            "Manual `/skills reload` has been retired (ADR-0165).\n\
                             Skills update automatically via reactive file watching.\n\
                             To authorize project skills in a new workspace, run `/trust skills`."
                                .to_string(),
                        ),
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
                            "Unknown skills command '{}'. Use 'list' or 'status'.",
                            other
                        ),
                    )
                    .await;
                }
            }
        }
        Some(BuiltinCmd::Skill) => {
            let rest = cmd.strip_prefix("/skill").unwrap_or("").trim();
            let mut iter = rest.split_whitespace();
            let first = iter.next().unwrap_or("");
            let (action, target_skill) = match first {
                "show" => ("show", iter.next().unwrap_or("")),
                "info" => ("info", iter.next().unwrap_or("")),
                "run" => ("run", iter.next().unwrap_or("")),
                other => ("run", other),
            };

            if target_skill.is_empty() {
                record_error(
                    session,
                    resp_tx,
                    name,
                    args,
                    "Usage: /skill <name> | /skill show <name> | /skill info <name>",
                )
                .await;
            } else if action == "info" {
                let info_opt = {
                    let guard = skills_registry_for_commands.lock();
                    guard.get(target_skill).map(|skill| {
                        let version = skill.version.as_deref().unwrap_or("–");
                        let status = if skill.quarantined {
                            "Quarantined (run /trust skills to enable)"
                        } else if skill.enabled {
                            "Enabled"
                        } else {
                            "Disabled"
                        };
                        let tags = if skill.tags.is_empty() {
                            "none".to_string()
                        } else {
                            skill.tags.join(", ")
                        };
                        format!(
                            "Skill: {}\n\
                             Scope: {}\n\
                             Version: {}\n\
                             Status: {}\n\
                             Tags: {}\n\
                             Source: {}\n\
                             Description: {}",
                            skill.name,
                            skill.scope,
                            version,
                            status,
                            tags,
                            skill.source.display(),
                            skill.description
                        )
                    })
                };
                if let Some(info) = info_opt {
                    record_command(session, resp_tx, name, args, CommandResult::Text(info)).await;
                } else {
                    record_error(
                        session,
                        resp_tx,
                        name,
                        args,
                        format!("Skill '{target_skill}' not found. Use `/skills` to see available skills."),
                    )
                    .await;
                }
            } else if action == "show" {
                let show_opt = {
                    let guard = skills_registry_for_commands.lock();
                    guard.get(target_skill).map(|skill| {
                        let body = skill.load_body().unwrap_or_default();
                        format!(
                            "# Skill: {} ({})\n\n{}\n\n---\n\n{}",
                            skill.name,
                            skill.scope,
                            skill.description,
                            body
                        )
                    })
                };
                if let Some(output) = show_opt {
                    record_command(session, resp_tx, name, args, CommandResult::Text(output)).await;
                } else {
                    record_error(
                        session,
                        resp_tx,
                        name,
                        args,
                        format!("Skill '{target_skill}' not found. Use `/skills` to see available skills."),
                    )
                    .await;
                }
            } else {
                let args_json = serde_json::json!({ "name": target_skill }).to_string();
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
            if refuse_if_no_provider(resp_tx, agent, session, &session.id().await).await {
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
