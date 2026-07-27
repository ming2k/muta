//! The driver for one live agent session.
//!
//! [`SessionDriver`] owns the request receiver and every long-lived dependency
//! needed to serve that session: the `Agent`, session store, configuration,
//! provider telemetry, `/btw` side session, cancellation state, and frontend
//! bridge. [`SessionDriver::run`] consumes the driver, dispatches each
//! [`AgentRequest`] to its handler, and exits when all request senders are
//! dropped.
//!
//! The implementation still destructures the driver into the local names used
//! by the original inline task. This keeps the dispatch body unchanged while
//! making its ownership boundary explicit.

use crate::commands::CustomCommand;
use neenee_agent::catalog;
use neenee_agent::orchestration::{round_response, send_harness_state};
use neenee_agent::{Agent, EnvoyRegistry, RoundLifecycle};
use neenee_core::{AgentRequest, AgentResponse, LoopStatus, Provider, Tool};
use neenee_agent::mcp::McpRuntime;
use neenee_persistence::{
    config::Config, embedding, provider_usage::ProviderUsage, session::SessionStore,
};
use neenee_skills::SkillRegistry;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock, atomic::AtomicBool};
use tokio::sync::{RwLock as AsyncRwLock, mpsc};

use crate::UiBridge;
use crate::session_view::{build_sessions_overview, provider_key_status};
use crate::side::SideSession;
use crate::startup::StartupMode;

/// The owned state and request loop for one live session.
///
/// A frontend currently assembles the driver after startup wiring and moves it
/// into a Tokio task. (The ADR-0037 §6 `SessionRegistry` factory was removed
/// as dormant; if the server move resumes, the fields can become private
/// without changing the driver model.)
#[allow(clippy::type_complexity)]
pub struct SessionDriver {
    /// Inbound requests consumed by this driver.
    pub req_rx: mpsc::UnboundedReceiver<AgentRequest>,
    /// Responses bound for the frontend (`resp_tx` in the old code).
    pub tx: mpsc::UnboundedSender<AgentResponse>,
    /// Inbound request sender, cloned so `/repeat` can self-fire a `Chat`
    /// (`req_tx_for_commands` in the old code).
    pub req_tx: mpsc::UnboundedSender<AgentRequest>,
    /// The primary agent.
    pub agent: Arc<Agent>,
    /// The primary session store.
    pub session: Arc<SessionStore>,
    /// Live config; mutated by provider/favorite/default switches and saved.
    pub config: Config,
    /// Per-model usage telemetry; mutated by activations and switches.
    pub provider_usage: ProviderUsage,
    /// The shared provider holder backing the `ProxyProvider`
    /// (`provider_for_task` in the old code).
    pub provider_holder: Arc<RwLock<Arc<dyn Provider>>>,
    /// Shared skills registry.
    pub skills_registry: Arc<SkillRegistry>,
    /// Full-duplex envoy registry (ADR-0029): maps the parent tool-call
    /// id to the live child handle so a permission / ask_user reply can be
    /// routed back down into the specific envoy that surfaced it.
    pub envoy_registry: Arc<EnvoyRegistry>,
    /// Live MCP runtime: the connected server set, their tools, and status.
    /// Mutated by the `/mcp` modal (toggle / reconnect) and the periodic
    /// catalog refresh; read for the session-context snapshot's MCP pane.
    pub mcp_runtime: Arc<McpRuntime>,
    /// User-defined `/<name>` commands (`commands_for_task` in the old code).
    pub commands: Arc<HashMap<String, CustomCommand>>,
    /// Project embedding index for `/search` (`embedding_store_for_commands`).
    pub embedding_store: Arc<AsyncRwLock<embedding::EmbeddingStore>>,
    /// Primary round lifecycle: at most one active round, superseded by the
    /// next begin (replaces the old token-slot + generation-counter pair).
    pub lifecycle: Arc<RoundLifecycle>,
    /// Live `/btw` side session registry (ADR-0017).
    pub side: Arc<AsyncRwLock<Option<SideSession>>>,
    /// Whether the user is composing into the side view right now.
    pub active_view_side: Arc<AtomicBool>,
    /// Cached base toolset snapshot for side-session construction
    /// (`base_tools_for_side`).
    pub base_tools: Arc<Vec<Arc<dyn Tool>>>,
    /// Project root for side-session pinning (`project_root_for_side`).
    pub project_root: PathBuf,
    /// Startup mode — read by the misplaced SessionStart-hooks block inside
    /// `/pursue status` (preserved verbatim; see note in [`Self::run`]).
    pub startup: StartupMode,
    /// Whether the sessions picker should open on launch.
    pub open_picker_on_start: bool,
    /// Frontend clipboard bridge (ADR-0037 step 3). The TUI provides a real
    /// impl; a future browser frontend provides its own. Used only by the
    /// `/export` slash command.
    pub ui: Arc<dyn UiBridge>,
    /// Shared token-source ledger (reported vs. estimated token accounting).
    /// Installed into `agent` once at startup; the TUI reads it for the
    /// token-source report modal.
    pub token_ledger: Arc<neenee_core::TokenSourceLedger>,
    /// Application-registered slash command handlers (the extension point for
    /// commands that run Rust logic, e.g. a sibling binary's custom command).
    /// The dispatcher consults this in its unknown-built-in arm
    /// before falling back to the markdown-template path. Empty for `neenee`
    /// today; populated by embeddings that need it.
    pub extra_commands: Arc<crate::slash_handler::SlashCommandRegistry>,
}

impl SessionDriver {
    /// Run the session to completion, exiting when the request channel closes.
    ///
    /// The driver is destructured into locals with the original inline-task
    /// names so the established dispatch body remains unchanged.
    //
    // NOTE: a `refresh_agent_pursuit` + SessionStart-hooks block inside the
    // `/pursue status` branch has inconsistent indentation and looks misplaced —
    // it fires session-start hooks every time `/pursue status` runs. Preserved
    // verbatim here; not this refactor's job to fix.
    pub async fn run(self) {
        let SessionDriver {
            mut req_rx,
            tx: resp_tx,
            req_tx: req_tx_for_commands,
            agent,
            session,
            mut config,
            mut provider_usage,
            provider_holder: provider_for_task,
            skills_registry,
            envoy_registry,
            mcp_runtime,
            commands: commands_for_task,
            embedding_store: embedding_store_for_commands,
            lifecycle,
            side,
            active_view_side,
            base_tools: base_tools_for_side,
            project_root: project_root_for_side,
            startup,
            open_picker_on_start,
            ui,
            token_ledger,
            extra_commands,
        } = self;
        // Hand the shared token-source ledger to the agent so each turn's token
        // usage (reported vs. estimated) is booked into it for the report modal.
        agent.install_token_ledger(token_ledger.clone());
        // The old inline block captured two clones of the skills registry —
        // `skills_registry` (read for the session-context snapshot) and
        // `skills_registry_for_commands` (handed to the `/skills` / `/skill`
        // tools). One driver field backs both; re-create the alias here.
        let skills_registry_for_commands = skills_registry.clone();

        let initial_session_id = session.id().await;
        token_ledger.restore_session(&initial_session_id, session.request_usage_records().await);
        token_ledger.set_active_session(initial_session_id.clone());
        let initial_context = agent
            .estimate_next_request_tokens(&session.model_window().await)
            .total_tokens;
        let _ = resp_tx.send(round_response(
            &initial_session_id,
            neenee_core::RoundEvent::ContextTokens(neenee_core::ContextTokenSnapshot {
                tokens: initial_context,
                source: neenee_core::ContextTokenSource::Projection,
            }),
        ));
        send_harness_state(&resp_tx, &initial_session_id, &agent, LoopStatus::Idle);
        let _ = resp_tx.send(AgentResponse::ProviderKeys(provider_key_status(&config)));
        // Record that the default provider + model were activated on startup, so
        // the picker's recency ordering reflects "last used = now" for both
        // stages, and the provider is pinned to the exact model it booted under.
        // Both signals are needed: `record` drives stage-1 provider ordering and
        // `record_model` drives stage-2 model ordering *and* writes the
        // `last_models` pin that `active_model_id_for_entry` consults on the next
        // launch to re-open the provider on its exact model instead of a
        // re-derived default. Recording only the provider (the previous behavior)
        // left `last_models` stale, so a session that booted into a provider —
        // never manually switched its model — reopened on the default-channel
        // model rather than the one it actually ran with. Best-effort: usage
        // tracking is rebuildable state and must never block startup.
        {
            let initial_id = catalog::default_provider_id(&config).to_string();
            // Resolve the model the way `build_provider_for` did when main.rs
            // constructed the startup provider: `config.default_model` when the
            // entry serves it, otherwise the entry's default-channel model. The
            // config-only resolver (`resolved_model_name`, *not* the `_with_usage`
            // variant) mirrors that precedence exactly — it ignores `last_models`,
            // so it never pins a model the live provider was not actually built
            // with. Pinning the exact live model (rather than a usage-derived one)
            // is what lets the next launch re-open this provider on the same model.
            let initial_model = catalog::resolved_model_name(&config, &initial_id);
            provider_usage.record(&initial_id);
            // Skip the model pin when the startup provider is unbuildable
            // (`resolved_model_name` returns `None`): there is no real channel,
            // so pinning a (non-existent) model would be a spurious `last_models`
            // entry. The provider recency bump above still runs so the picker
            // ordering is correct.
            if let Some(model) = initial_model.as_deref() {
                provider_usage.record_model(&initial_id, model);
            }
            if let Err(error) = provider_usage.save() {
                tracing::warn!(?error, "could not persist provider/model usage telemetry");
            }
        }
        // Push the initial model-picker snapshot (default id + per-model
        // favorite / key-ready / last-used) so the picker is ready the moment
        // the user opens it.
        let _ = resp_tx.send(AgentResponse::ProviderPicker(catalog::build_picker_state(
            &config,
            &provider_usage,
        )));
        if open_picker_on_start {
            let _ = resp_tx.send(AgentResponse::SessionsOverview(
                build_sessions_overview(&session).await,
            ));
        }
        while let Some(req) = req_rx.recv().await {
            let pre_session_id = session.id().await;
            let pre_projection = agent
                .estimate_next_request_tokens(&session.model_window().await)
                .total_tokens;
            let pre_provider = agent.provider.provider_id();
            let pre_model = agent.provider.model();
            match req {
                AgentRequest::Interrupt => {
                    crate::handlers_permission::interrupt(&agent, &session, &resp_tx, &lifecycle)
                        .await;
                }
                AgentRequest::PermissionReply {
                    request_id,
                    decision,
                    parent_call_id,
                } => {
                    crate::handlers_permission::reply(
                        &agent,
                        &envoy_registry,
                        &resp_tx,
                        request_id,
                        decision,
                        parent_call_id,
                    )
                    .await;
                }
                AgentRequest::UserQuestionReply {
                    request_id,
                    answers,
                    parent_call_id,
                } => {
                    crate::handlers_permission::reply_question(
                        &agent,
                        &envoy_registry,
                        &side,
                        &resp_tx,
                        request_id,
                        answers,
                        parent_call_id,
                    )
                    .await;
                }
                AgentRequest::InputReply {
                    request_id,
                    text,
                    parent_call_id,
                } => {
                    crate::handlers_permission::reply_input(
                        &agent,
                        &envoy_registry,
                        &side,
                        &resp_tx,
                        request_id,
                        text,
                        parent_call_id,
                    )
                    .await;
                }
                AgentRequest::SwitchProvider {
                    provider_type,
                    model,
                    api_key,
                    base_url,
                } => {
                    crate::handlers_provider::switch(
                        &mut config,
                        &agent,
                        &provider_for_task,
                        &session,
                        &resp_tx,
                        &mut provider_usage,
                        provider_type,
                        model,
                        api_key,
                        base_url,
                    )
                    .await;
                }
                AgentRequest::AddProvider {
                    name,
                    protocol,
                    base_url,
                    api_key,
                    user_agent,
                    models,
                    auth,
                    template_id,
                } => {
                    crate::handlers_provider::add(
                        &mut config,
                        &agent,
                        &provider_for_task,
                        &session,
                        &resp_tx,
                        &mut provider_usage,
                        name,
                        protocol,
                        base_url,
                        api_key,
                        user_agent,
                        models,
                        auth,
                        template_id,
                    )
                    .await;
                }
                AgentRequest::ConnectProvider { id, method } => {
                    crate::handlers_provider::connect(
                        &mut config,
                        &agent,
                        &provider_for_task,
                        &resp_tx,
                        &mut provider_usage,
                        id,
                        method,
                    )
                    .await;
                }
                AgentRequest::AuthorizeOAuth { method, auth } => {
                    crate::handlers_provider::authorize(&resp_tx, method, auth).await;
                }
                AgentRequest::EditProvider {
                    id,
                    name,
                    protocol,
                    base_url,
                    api_key,
                } => {
                    crate::handlers_provider::edit(
                        &mut config,
                        &agent,
                        &provider_for_task,
                        &resp_tx,
                        &mut provider_usage,
                        id,
                        name,
                        protocol,
                        base_url,
                        api_key,
                    )
                    .await;
                }
                AgentRequest::RemoveProviderModel { provider_id, model } => {
                    crate::handlers_provider::remove_model(
                        &mut config,
                        &resp_tx,
                        &provider_usage,
                        provider_id,
                        model,
                    )
                    .await;
                }
                AgentRequest::EditProviderModel {
                    provider_id,
                    model,
                    effort,
                    thinking,
                } => {
                    crate::handlers_provider::edit_model(
                        &mut config,
                        &agent,
                        &provider_for_task,
                        &resp_tx,
                        &mut provider_usage,
                        provider_id,
                        model,
                        effort,
                        thinking,
                    )
                    .await;
                }
                AgentRequest::EditModelReasoning {
                    model,
                    effort,
                    thinking,
                } => {
                    crate::handlers_provider::edit_model_reasoning(
                        &mut config,
                        &agent,
                        &provider_for_task,
                        &resp_tx,
                        &mut provider_usage,
                        model,
                        effort,
                        thinking,
                    )
                    .await;
                }
                AgentRequest::DeleteProvider { id } => {
                    crate::handlers_provider::delete(
                        &mut config,
                        &agent,
                        &provider_for_task,
                        &resp_tx,
                        &mut provider_usage,
                        id,
                    )
                    .await;
                }
                AgentRequest::ToggleFavorite { id } => {
                    crate::handlers_provider::toggle_favorite(
                        &mut config,
                        &resp_tx,
                        &provider_usage,
                        id,
                    )
                    .await;
                }
                AgentRequest::SetDefaultModel { id } => {
                    crate::handlers_provider::set_default_model(
                        &mut config,
                        &agent,
                        &provider_for_task,
                        &resp_tx,
                        &mut provider_usage,
                        id,
                    )
                    .await;
                }
                AgentRequest::DeleteSession { id } => {
                    crate::handlers_session::delete(&session, &resp_tx, id).await;
                }
                AgentRequest::QuerySessionContext => {
                    crate::handlers_session::query_context(
                        &agent,
                        &skills_registry,
                        &mcp_runtime,
                        &config,
                        &resp_tx,
                    );
                }
                AgentRequest::RevokePermission { tool, scope } => {
                    crate::handlers_session::revoke_permission(
                        &agent,
                        &skills_registry,
                        &mcp_runtime,
                        &config,
                        &resp_tx,
                        tool,
                        scope,
                    );
                }
                AgentRequest::ClearAllPermissions => {
                    crate::handlers_session::clear_all_permissions(
                        &agent,
                        &skills_registry,
                        &mcp_runtime,
                        &config,
                        &resp_tx,
                    );
                }
                AgentRequest::ToggleTool { name, enabled } => {
                    crate::handlers_session::toggle_tool(
                        &agent,
                        &skills_registry,
                        &mcp_runtime,
                        &config,
                        &resp_tx,
                        name,
                        enabled,
                    );
                }
                AgentRequest::ToggleMcpServer { name, enabled } => {
                    crate::handlers_session::toggle_mcp_server(
                        &agent,
                        &skills_registry,
                        &mcp_runtime,
                        &config,
                        &resp_tx,
                        name,
                        enabled,
                    )
                    .await;
                }
                AgentRequest::ReconnectMcpServer { name } => {
                    crate::handlers_session::reconnect_mcp_server(
                        &agent,
                        &skills_registry,
                        &mcp_runtime,
                        &config,
                        &resp_tx,
                        name,
                    )
                    .await;
                }
                AgentRequest::SlashCommand(cmd) => {
                    crate::handlers_slash::dispatch(
                        cmd,
                        &config,
                        &agent,
                        &resp_tx,
                        &session,
                        &lifecycle,
                        &side,
                        &active_view_side,
                        &base_tools_for_side,
                        &provider_for_task,
                        &mut provider_usage,
                        skills_registry.clone(),
                        &skills_registry_for_commands,
                        &commands_for_task,
                        &embedding_store_for_commands,
                        &req_tx_for_commands,
                        &project_root_for_side,
                        &startup,
                        &*ui,
                        &extra_commands,
                    )
                    .await;
                }
                AgentRequest::Chat {
                    text,
                    images,
                    sent_at_ms,
                } => {
                    crate::handlers_chat::chat(
                        &active_view_side,
                        &side,
                        &agent,
                        &session,
                        &lifecycle,
                        &resp_tx,
                        &config,
                        text,
                        images,
                        sent_at_ms,
                    )
                    .await;
                }
                AgentRequest::InsertUserInput { session_id, input } => {
                    crate::handlers_chat::insert_user_input(
                        &side, &agent, &session, &resp_tx, session_id, input,
                    )
                    .await;
                }
                AgentRequest::CancelInsertedInput {
                    session_id,
                    input_id,
                } => {
                    crate::handlers_chat::cancel_inserted_input(
                        &side, &agent, &session, &resp_tx, session_id, input_id,
                    )
                    .await;
                }
                AgentRequest::ChatToSession { session_id, input } => {
                    crate::handlers_chat::chat_to_session(
                        &side, &agent, &session, &lifecycle, &resp_tx, &config, session_id, input,
                    )
                    .await;
                }
                AgentRequest::ShellCommand { command } => {
                    crate::handlers_chat::shell(&resp_tx, &lifecycle, &agent, &session, command)
                        .await;
                }
                AgentRequest::ExitSideView => {
                    crate::handlers_session::exit_side_view(&side, &active_view_side, &resp_tx)
                        .await;
                }
                AgentRequest::UpdateTuiLayout(layout) => {
                    // Persist the updated transcript layout preference. This
                    // is not a selection change, so preserve the on-disk
                    // provider/model selection rather than leaking the
                    // in-memory (possibly session-pinned) one into config.toml.
                    config.tui.transcript_layout = layout.clone();
                    if let Err(error) = config.save_preserving_provider_selection() {
                        let _ = resp_tx.send(AgentResponse::Error(format!(
                            "Could not save transcript layout: {error}"
                        )));
                    } else {
                        let _ = resp_tx.send(AgentResponse::TuiLayoutUpdated(layout));
                    }
                }
                AgentRequest::UpdateTuiColorScheme { name, custom } => {
                    // Persist both pieces atomically. The custom palette remains
                    // available when the active selection is a built-in preset.
                    // Preserve the on-disk provider/model selection (see the
                    // layout handler above).
                    config.tui.color_scheme = name.clone();
                    config.tui.custom_color_scheme = custom.clone();
                    if let Err(error) = config.save_preserving_provider_selection() {
                        let _ = resp_tx.send(AgentResponse::Error(format!(
                            "Could not save color scheme: {error}"
                        )));
                    } else {
                        let _ = resp_tx.send(AgentResponse::TuiColorSchemeUpdated { name, custom });
                    }
                }
            }

            // Compare against the post-dispatch projection and only re-publish
            // when the AI-visible context changed.
            let post_session_id = session.id().await;
            let post_projection = agent
                .estimate_next_request_tokens(&session.model_window().await)
                .total_tokens;
            let provider_or_model_changed =
                pre_provider != agent.provider.provider_id() || pre_model != agent.provider.model();
            let session_changed = post_session_id != pre_session_id;

            if session_changed {
                token_ledger
                    .restore_session(&post_session_id, session.request_usage_records().await);
                token_ledger.set_active_session(post_session_id.clone());
                agent.set_thread_id(post_session_id.clone());
                agent.restore_round_count(session.round_counter().await);
            }

            // Re-publish a session-scoped projection only when the AI-visible
            // context actually changed this request (session switch, `/clear`,
            // `/compact`, provider/tool/skill change, …). Comparing the pre-
            // and post-dispatch estimates — rather than enumerating request
            // variants — keeps a non-driving command echo (or any no-op
            // request) from overwriting a fresh provider-reported context
            // anchor with the lower local estimate.
            if session_changed || provider_or_model_changed || post_projection != pre_projection {
                let _ = resp_tx.send(round_response(
                    &post_session_id,
                    neenee_core::RoundEvent::ContextTokens(neenee_core::ContextTokenSnapshot {
                        tokens: post_projection,
                        source: neenee_core::ContextTokenSource::Projection,
                    }),
                ));
            }
        }
    }
}
