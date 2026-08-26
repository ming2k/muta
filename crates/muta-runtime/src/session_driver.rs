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
use muta_agent::catalog;
use muta_agent::orchestration::{round_response, send_harness_state};
use muta_agent::{Agent, RoundLifecycle, RunnerRegistry};
use muta_contracts::{AgentRequest, AgentResponse, LoopStatus, Provider, Tool};
use muta_mcp::McpRuntime;
use muta_persistence::{
    config::Config, connection_usage::ConnectionUsage, embedding, session::SessionStore,
    workspace_security::WorkspaceSecurityStore,
};
use muta_skills::SkillRegistry;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use tokio::sync::{RwLock as AsyncRwLock, mpsc};

use crate::UiBridge;
use crate::session_view::{build_sessions_overview, provider_key_status};
use crate::startup::SessionStart;

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
    pub provider_usage: ConnectionUsage,
    /// The shared provider holder backing the `ProxyProvider`
    /// (`provider_for_task` in the old code).
    pub provider_holder: Arc<RwLock<Arc<dyn Provider>>>,
    /// Shared skills registry.
    pub skills_registry: Arc<SkillRegistry>,
    /// Full-duplex runner registry (ADR-0029): maps the parent tool-call
    /// id to the live child handle so a permission / ask_user reply can be
    /// routed back down into the specific runner that surfaced it.
    pub runner_registry: Arc<RunnerRegistry>,
    /// Live MCP runtime: the connected server set, their tools, and status.
    /// Mutated by the `/mcp` modal (toggle / reconnect) and the periodic
    /// catalog refresh; read for the session-context snapshot's MCP pane.
    pub mcp_runtime: Arc<McpRuntime>,
    /// Workspace execution authority and content-bound extension trust.
    pub workspace_security: Arc<WorkspaceSecurityStore>,
    /// User-defined `/<name>` commands (`commands_for_task` in the old code).
    pub commands: Arc<HashMap<String, CustomCommand>>,
    /// Backend-owned command vocabulary used by both attach metadata and the
    /// composer completion engine.
    pub command_catalog: muta_contracts::CommandCatalog,
    /// Project embedding index for `/search` (`embedding_store_for_commands`).
    pub embedding_store: Arc<AsyncRwLock<embedding::EmbeddingStore>>,
    /// Primary round lifecycle: at most one active round, superseded by the
    /// next begin (replaces the old token-slot + generation-counter pair).
    pub lifecycle: Arc<RoundLifecycle>,
    /// Live `/btw` aside registry (ADR-0017, multi-slot per ADR-0103).
    pub side: Arc<AsyncRwLock<crate::side::SideRegistry>>,
    /// Cached base toolset snapshot for side-session construction
    /// (`base_tools_for_side`).
    pub base_tools: Arc<Vec<Arc<dyn Tool>>>,
    /// Project root for side-session pinning (`project_root_for_side`).
    pub project_root: PathBuf,
    /// Startup mode — read by the misplaced SessionStart-hooks block inside
    /// `/pursue status` (preserved verbatim; see note in [`Self::run`]).
    pub startup: SessionStart,
    /// Whether the sessions picker should open on launch (`mutx attach`
    /// with no id).
    pub open_picker_on_start: bool,
    /// Frontend clipboard bridge (ADR-0037 step 3). The TUI provides a real
    /// impl; a future browser frontend provides its own. Used only by the
    /// `/export` slash command.
    pub ui: Arc<dyn UiBridge>,
    /// Shared token-source ledger (reported vs. estimated token accounting).
    /// Installed into `agent` once at startup; the TUI reads it for the
    /// token-source report modal.
    pub token_ledger: Arc<muta_contracts::TokenSourceLedger>,
    /// Application-registered slash command handlers (the extension point for
    /// commands that run Rust logic, e.g. a sibling binary's custom command).
    /// The dispatcher consults this in its unknown-built-in arm
    /// before falling back to the markdown-template path. Empty for `muta`
    /// today; populated by embeddings that need it.
    pub extra_commands: Arc<crate::slash_handler::SlashCommandRegistry>,
    /// Shared hot-reloadable `[websearch]` configuration. The web tools hold
    /// the same handle; `UpdateWebSearchConfig` and `/settings reload` write
    /// into it so provider/reader/proxy changes take effect on the next tool
    /// call without rebuilding the toolset.
    pub websearch_shared: Arc<muta_contracts::SharedWebSearchConfig>,
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
            runner_registry,
            mcp_runtime,
            workspace_security,
            commands: commands_for_task,
            command_catalog,
            embedding_store: embedding_store_for_commands,
            lifecycle,
            side,
            base_tools: base_tools_for_side,
            project_root: project_root_for_side,
            startup,
            open_picker_on_start,
            ui,
            token_ledger,
            extra_commands,
            websearch_shared,
        } = self;
        // Hand the shared token-source ledger to the agent so each turn's token
        // usage (reported vs. estimated) is booked into it for the report modal.
        agent.install_token_ledger(token_ledger.clone());
        // The old inline block captured two clones of the skills registry —
        // `skills_registry` (read for the session-context snapshot) and
        // `skills_registry_for_commands` (handed to the `/skills` / `/skill`
        // tools). One driver field backs both; re-create the alias here.
        let skills_registry_for_commands = skills_registry.clone();
        let completion_engine = crate::input_completion::InputCompletionEngine::new(
            command_catalog,
            project_root_for_side.clone(),
        );

        let initial_session_id = session.id().await;
        token_ledger.restore_session(&initial_session_id, session.request_usage_records().await);
        token_ledger.set_active_session(initial_session_id.clone());
        // Crash-residue recovery (ADR-0128). The round path arms the durable
        // `/retry` resume point only on stops it can observe — a terminal
        // error after the provider retry budget, an interrupt past the
        // phase-1 unsend window. A process that dies with a round on the
        // wire (SIGKILL, panic, power loss) runs none of those paths, so
        // the point is never armed and a resumed session answers `/retry`
        // with "Nothing to retry" even though its last round visibly died
        // mid-flight.
        //
        // The reliable residue marker is a request-usage record that is
        // still `InFlight` in the *session store*. Every live settlement
        // path writes a terminal status back before the round ends, and
        // `TokenSourceLedger::restore_session` (called just above) flips
        // the copies it loads to `Abandoned` — but only in the ledger's
        // in-memory map, never in the store. So the store's own `InFlight`
        // means "nobody ever settled this request", and reading it here —
        // before any new round can rewrite the ledger — is exactly the
        // crash signal. (The old comment claimed `restore_session` had
        // already flipped the store copy; it had not, which is why the
        // previous `Abandoned` filter never fired on the first reload and
        // could only ever fire on stale records from an *earlier* round
        // after a resume-then-crash-again sequence — the opposite of the
        // intent.)
        {
            let residue = recover_crashed_round(
                &session,
                session.request_usage_records().await,
                crate::registry::unix_epoch_ms(),
            )
            .await;
            for record in residue.interrupts {
                if let Err(error) = session.record_round_interrupt(record).await {
                    tracing::warn!(?error, "could not record crash-residue interrupt");
                }
            }
            if let Some(point) = residue.retry_point {
                tracing::info!(
                    session = %initial_session_id,
                    round = point.round,
                    "armed crash-resume /retry point for the terminated round"
                );
                if let Err(error) = session.arm_retry_pending(point).await {
                    tracing::warn!(%error, "could not arm crash-resume retry point");
                }
            }
        }
        let initial_context = agent
            .estimate_next_request_tokens(&session.model_window().await)
            .total_tokens;
        let _ = resp_tx.send(round_response(
            &initial_session_id,
            muta_contracts::RoundEvent::ContextTokens(muta_contracts::ContextTokenSnapshot::new(
                initial_context,
                muta_contracts::ContextTokenSource::Projection,
            )),
        ));
        // Session-scoped idle snapshot (ADR-0128): publishes the `/retry`
        // affordance from the durable resume point so a session whose round
        // stopped before a detach/reattach offers `/retry` from frame one.
        muta_agent::orchestration::send_harness_state_for_session(
            &resp_tx,
            &initial_session_id,
            &agent,
            &session,
            LoopStatus::Idle,
        )
        .await;
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
        catalog::prune_stale_models(&mut config, &mut provider_usage);
        // Push the initial model-picker snapshot (default id + per-model
        // favorite / key-ready / last-used) so the picker is ready the moment
        // the user opens it.
        let _ = resp_tx.send(AgentResponse::ProviderPicker(catalog::build_picker_state(
            &config,
            &provider_usage,
        )));
        // Announce the active provider/model as a synthetic `ProviderSwitched`
        // so an attach client (which subscribes to the broadcast only after the
        // handshake and so misses the startup emissions) can seed its hint bar
        // — model name, reasoning effort, `@instance`, context meter — from the
        // same single source the in-process TUI reads. The driver resolved this
        // pair from the global default overlaid with the session's provider pin
        // (C6), so it is authoritative for this session. Emitting it here (after
        // the picker snapshot) also lets the registry's attach-sync buffer
        // capture and replay it. Resolved config-only (`resolved_model_name`,
        // not the usage variant) to mirror exactly the model the live provider
        // was built with.
        {
            let provider = catalog::default_provider_id(&config).to_string();
            let model = catalog::resolved_model_name(&config, &provider).unwrap_or_default();
            let _ = resp_tx.send(AgentResponse::ProviderSwitched { provider, model });
        }
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
            // Requests that own the round lifecycle close their own activity
            // resolution: the round task (or the shell-command round) always
            // emits a terminal `HarnessState(Idle)` on exit. Every other
            // request is a control-plane op that does not. The TUI no longer
            // arms optimistic activity state for control-plane dispatches at
            // all (ADR-0110: a command is outside the round state machine),
            // but other frontends may still paint their own optimistic
            // state, so the driver keeps reconciling every non-round request
            // back to the authoritative harness state — see the reconcile
            // below the match and ADR-0091/0110.
            let reconcile_activity = needs_activity_reconcile(&req, &lifecycle).await;
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
                        &runner_registry,
                        &side,
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
                        &runner_registry,
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
                        &runner_registry,
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
                    client_identity,
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
                        client_identity,
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
                    client_identity,
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
                        client_identity,
                    )
                    .await;
                }
                AgentRequest::RemoveProviderModel { provider_id, model } => {
                    crate::handlers_provider::remove_model(
                        &mut config,
                        &resp_tx,
                        &mut provider_usage,
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
                    overrides,
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
                        overrides,
                    )
                    .await;
                }
                AgentRequest::EditModelReasoning {
                    model,
                    effort,
                    thinking,
                    overrides,
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
                        overrides,
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
                AgentRequest::RefreshProviderModels { user_initiated } => {
                    crate::handlers_provider::refresh_models(
                        &mut config,
                        &agent,
                        &provider_for_task,
                        &resp_tx,
                        &mut provider_usage,
                        Some(&session),
                        user_initiated,
                    )
                    .await;
                }
                AgentRequest::DeleteSession { id } => {
                    let session = session.clone();
                    let resp_tx = resp_tx.clone();
                    let embedding_store = embedding_store_for_commands.clone();
                    tokio::spawn(async move {
                        crate::handlers_session::delete(&session, &embedding_store, &resp_tx, id)
                            .await;
                    });
                }
                AgentRequest::RenameSession { id, title } => {
                    let session = session.clone();
                    let resp_tx = resp_tx.clone();
                    tokio::spawn(async move {
                        crate::handlers_session::rename(&session, &resp_tx, id, title).await;
                    });
                }
                AgentRequest::QuerySessionDetail { id } => {
                    let session = session.clone();
                    let resp_tx = resp_tx.clone();
                    tokio::spawn(async move {
                        crate::handlers_session::detail(&session, &resp_tx, id).await;
                    });
                }
                AgentRequest::QuerySessionsOverview => {
                    let session = session.clone();
                    let resp_tx = resp_tx.clone();
                    tokio::spawn(async move {
                        crate::handlers_session::overview(&session, &resp_tx).await;
                    });
                }
                AgentRequest::QuerySessionTree => {
                    // Keep id + tree capture ordered with session-switch
                    // requests handled by this driver, so the tagged snapshot
                    // can never pair one session's id with another's DAG.
                    crate::handlers_session::tree(&session, &resp_tx).await;
                }
                AgentRequest::QueryTokenUsage { session_id } => {
                    crate::handlers_session::token_usage(&token_ledger, &resp_tx, session_id);
                }
                AgentRequest::QueryUsageStats { event_cap } => {
                    crate::handlers_session::usage_stats(&resp_tx, event_cap);
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
                        &mcp_runtime,
                        &workspace_security,
                        &resp_tx,
                        &session,
                        &lifecycle,
                        &side,
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
                        &websearch_shared,
                    )
                    .await;
                }
                AgentRequest::CompleteInput {
                    request_id,
                    input,
                    cursor,
                } => {
                    let _ =
                        resp_tx.send(completion_engine.complete(request_id, input, cursor).await);
                }
                AgentRequest::Chat {
                    text,
                    images,
                    sent_at_ms,
                } => {
                    crate::handlers_chat::chat(
                        &side, &agent, &session, &lifecycle, &resp_tx, &config, text, images,
                        sent_at_ms,
                    )
                    .await;
                }
                AgentRequest::Steer { session_id, input } => {
                    crate::handlers_chat::steer(
                        &side, &agent, &session, &resp_tx, session_id, input,
                    )
                    .await;
                }
                AgentRequest::CancelSteer {
                    session_id,
                    input_id,
                } => {
                    crate::handlers_chat::cancel_steer(
                        &side, &agent, &session, &resp_tx, session_id, input_id,
                    )
                    .await;
                }
                AgentRequest::FollowUp { session_id, input } => {
                    crate::handlers_chat::follow_up(
                        &side, &agent, &session, &lifecycle, &resp_tx, &config, session_id, input,
                    )
                    .await;
                }
                AgentRequest::ShellCommand { command } => {
                    crate::handlers_chat::shell(
                        &resp_tx,
                        &lifecycle,
                        &agent,
                        &session,
                        &project_root_for_side,
                        command,
                    )
                    .await;
                }
                AgentRequest::ExitSideView => {
                    crate::handlers_session::detach_side_view(&side, &resp_tx).await;
                }
                AgentRequest::FocusSide { side_id } => {
                    crate::handlers_session::focus_side(&side, &session, &resp_tx, side_id).await;
                }
                AgentRequest::InterruptSide { side_id } => {
                    crate::handlers_session::interrupt_side(&side, &resp_tx, side_id).await;
                }
                AgentRequest::CloseSide { side_id } => {
                    crate::handlers_session::close_side(&side, &resp_tx, side_id).await;
                }
                AgentRequest::QueryBtwList => {
                    crate::side::publish_btw_list(&side, &resp_tx).await;
                }
                AgentRequest::UpdateTuiLayout(layout) => {
                    let _ = resp_tx.send(AgentResponse::TuiLayoutUpdated(layout));
                }
                AgentRequest::UpdateTuiColorScheme { name, custom } => {
                    let _ = resp_tx.send(AgentResponse::TuiColorSchemeUpdated { name, custom });
                }
                AgentRequest::QueryWebSearchConfig => {
                    crate::handlers_websearch::query(&config, &resp_tx);
                }
                AgentRequest::UpdateWebSearchConfig(update) => {
                    crate::handlers_websearch::update(
                        &mut config,
                        &websearch_shared,
                        *update,
                        &resp_tx,
                    )
                    .await;
                }
                AgentRequest::EndSession => {
                    // Unreachable in the normal topology: the WS attach path
                    // intercepts `EndSession` at the connection layer
                    // (serve.rs) precisely so it cannot queue behind work
                    // the teardown is about to cancel. This arm exists only
                    // for completeness / future in-process embedders.
                    tracing::warn!(
                        "session_driver: EndSession reached the driver queue; the \
                         connection layer should have intercepted it"
                    );
                }
            }

            // ── Activity-state reconcile (ADR-0091) ──────────────────────
            // Round-owned requests resolve themselves via the round task's
            // terminal `HarnessState(Idle)`. Control-plane requests must be
            // resolved here instead: re-publish the authoritative harness
            // state now that the handler has run. When a round is live the
            // reconcile is a no-op (the round's own events own the display —
            // and re-emitting a running snapshot would reset the TUI's round
            // timer/turn counters); when idle it is `HarnessState(Idle)`,
            // which the TUI maps to "collapse the activity bar". This keeps
            // "every dispatched request lands the harness back in its
            // authoritative state" a structural invariant regardless of what
            // a frontend optimistically painted (the TUI itself no longer
            // arms anything for control-plane dispatches — ADR-0110 — so for
            // it this reconcile is a no-op safety net).
            if reconcile_activity {
                send_harness_state(&resp_tx, &session.id().await, &agent, LoopStatus::Idle);
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
            // context actually changed this request (session switch, `/new`,
            // `/compact`, provider/tool/skill change, …). Comparing the pre-
            // and post-dispatch estimates — rather than enumerating request
            // variants — keeps a non-driving command echo (or any no-op
            // request) from overwriting a fresh provider-reported context
            // anchor with the lower local estimate.
            if session_changed || provider_or_model_changed || post_projection != pre_projection {
                let _ = resp_tx.send(round_response(
                    &post_session_id,
                    muta_contracts::RoundEvent::ContextTokens(
                        muta_contracts::ContextTokenSnapshot::new(
                            post_projection,
                            muta_contracts::ContextTokenSource::Projection,
                        ),
                    ),
                ));
            }
        }
    }
}

/// Whether `req` owns the round lifecycle and therefore resolves the TUI's
/// optimistic "queued" activity state on its own, via the round task's
/// terminal `HarnessState(Idle)`.
///
/// - Chat-family requests start (or feed) a round; the round task emits the
///   closing idle snapshot when it finishes, errors, or is interrupted.
/// - `ShellCommand` mirrors `start_interactive_round`: it begins its own
///   round and emits the terminal idle snapshot on exit.
///
/// Everything else is a control-plane operation (slash command, provider/
/// session/tool/mcp toggle, query, layout update, …) that runs inline in the
/// driver loop and emits no lifecycle event of its own. The driver
/// reconciles those after dispatch (see [`SessionDriver::run`]) by
/// re-publishing the authoritative harness state.
fn round_owned_request(req: &AgentRequest) -> bool {
    matches!(
        req,
        AgentRequest::Chat { .. }
            | AgentRequest::FollowUp { .. }
            | AgentRequest::Steer { .. }
            | AgentRequest::CancelSteer { .. }
            | AgentRequest::ShellCommand { .. }
    )
}

/// Whether the driver must reconcile the TUI's optimistic activity state after
/// dispatching `req`: true for every control-plane (non-round) request when no
/// round is live. When a round is running the reconcile is deliberately a
/// no-op — the round's own events own the display, and re-emitting a running
/// snapshot would reset the TUI's round timer/turn counters (ADR-0091).
async fn needs_activity_reconcile(req: &AgentRequest, lifecycle: &RoundLifecycle) -> bool {
    !matches!(req, AgentRequest::CompleteInput { .. })
        && !round_owned_request(req)
        && !lifecycle.is_running().await
}

/// The durable residue of one round the host process abandoned mid-flight.
#[derive(Debug, Default)]
struct CrashResidue {
    /// `Terminated` interrupt records to append (C11), one per distinct
    /// in-flight round, so the resumed transcript explains its dangling
    /// round instead of leaving it unexplained.
    interrupts: Vec<muta_contracts::RoundInterrupt>,
    /// A `/retry` resume point for the highest in-flight round, so a session
    /// re-hosted after a crash offers `/retry` instead of answering
    /// "Nothing to retry" (ADR-0128).
    retry_point: Option<muta_contracts::RetryPoint>,
}

/// Decide, from durable state alone, what a hard process death left dangling
/// (ADR-0128 + C11).
///
/// The crash signal is a request-usage record still `InFlight` **in the
/// session store**: every live settlement path (`RequestAccountingGuard`'s
/// Drop on completion/interrupt/failure) rewrites a terminal status through
/// `set_request_usage_records` before the round ends, and a graceful daemon
/// kill records a `Terminated` interrupt instead. A store-side `InFlight`
/// record therefore means the process vanished with the request on the wire.
///
/// Guards:
/// - Only the *highest* in-flight round is considered. The handler and
///   `start_resolved_turn` reject a point whose `round` no longer equals the
///   session's counter, so a lower one could never fire anyway.
/// - The point names only the *master* actor's round. Runner (`task`)
///   agents bill their own requests under `runner:<call-id>` against the same
///   session; a child's key must not decide the master's resume point.
/// - `turns_committed` is recovered from the transcript itself: the round's
///   committed turns are the assistant messages after its opening prompt
///   (the last visible, non-echo user message — the transcript carries no
///   round delimiters), because the in-flight turn was never committed.
/// - No point unless the round is the session's *current* one — the counter
///   below the record means the session moved past it (nothing to resurrect),
///   above it means the counter's durable write never landed.
///
/// A pre-existing terminal interrupt for the round does **not** suppress the
/// point. The two are orthogonal: the record explains the transcript, the
/// point offers recovery — and every stop a *graceful* path can observe
/// (interrupt, failure, completion) settles the usage record to a terminal
/// status, leaving store-side `InFlight` exclusively to process death. A
/// graceful kill therefore leaves the same residue as a crash, and its round
/// is just as resumable; suppressing on the interrupt would also break a
/// crash during a `/retry` resume, which reuses the same round number.
///
/// Performs only read access on the store; the caller applies the
/// interrupts / resume point through the store's normal durable setters.
async fn recover_crashed_round(
    session: &Arc<SessionStore>,
    records: Vec<muta_contracts::RequestUsageRecord>,
    now_ms: u64,
) -> CrashResidue {
    use muta_contracts::{RequestUsageStatus, Role};
    let mut residue = CrashResidue::default();
    let Some(latest) = records
        .iter()
        .filter(|record| record.status == RequestUsageStatus::InFlight)
        .max_by_key(|record| record.key.round)
    else {
        return residue;
    };
    let round = latest.key.round;
    residue.interrupts.push(muta_contracts::RoundInterrupt {
        reason: muta_contracts::RoundInterruptReason::Terminated,
        at_ms: now_ms,
        round: Some(round),
    });
    // Counter guard: the point may only name the session's *current* round —
    // the handler (and `start_resolved_turn`) reject anything else, and a
    // round below the counter means the session already moved past it (a
    // resume-then-more-work history), while a round above it means the
    // counter's durable write never landed.
    let round_counter = session.round_counter().await;
    if round != round_counter {
        return residue;
    }
    let window = session.model_window().await;
    // Committed ReAct turns of the crashed round. The transcript carries no
    // round delimiters, so the round's opener is approximated as the last
    // visible, non-echo user message: every turn this round committed follows
    // its opening prompt, and the in-flight one was never committed (its
    // partial stream died with the process). A hidden round input opens its
    // round the same way, while a command echo (`/cmd`, `!cmd`) is
    // non-driving and must not be taken for an opener. Mid-round
    // `InsertUserInput` admissions make this an undercount (the ordinal
    // resumes lower than reality) — cosmetic: the number only labels the
    // transcript band and usage keys, history itself is seeded from the exact
    // `history_watermark`.
    let opener = window
        .iter()
        .rposition(|message| {
            matches!(message.role, Role::User) && !message.hidden && !message.is_command_echo()
        })
        .map(|index| index + 1)
        .unwrap_or(0);
    let turns_committed = window[opener..]
        .iter()
        .filter(|message| matches!(message.role, Role::Assistant))
        .count();
    residue.retry_point = Some(muta_contracts::RetryPoint {
        round,
        turns_committed,
        history_watermark: window.len(),
        paused_ms: 0,
        at_ms: now_ms,
    });
    residue
}

#[cfg(test)]
mod tests {
    use super::*;
    use muta_agent::RoundLifecycle;
    use muta_contracts::{Message, RequestUsageStatus, Role};
    use std::sync::Arc;

    fn image() -> muta_contracts::ImagePart {
        muta_contracts::ImagePart {
            mime: "image/png".to_string(),
            data: "AAAA".to_string(),
        }
    }

    #[test]
    fn round_owned_requests_close_their_own_activity_lifecycle() {
        assert!(round_owned_request(&AgentRequest::Chat {
            text: "hi".to_string(),
            images: vec![image()],
            sent_at_ms: Some(1),
        }));
        assert!(round_owned_request(&AgentRequest::FollowUp {
            session_id: "s".to_string(),
            input: muta_contracts::QueuedMessage {
                id: "i".to_string(),
                text: "hi".to_string(),
                display_text: None,
                images: Vec::new(),
                sent_at_ms: None,
            },
        }));
        assert!(round_owned_request(&AgentRequest::Steer {
            session_id: "s".to_string(),
            input: muta_contracts::QueuedMessage {
                id: "i".to_string(),
                text: "hi".to_string(),
                display_text: None,
                images: Vec::new(),
                sent_at_ms: None,
            },
        }));
        assert!(round_owned_request(&AgentRequest::CancelSteer {
            session_id: "s".to_string(),
            input_id: "i".to_string(),
        }));
        assert!(round_owned_request(&AgentRequest::ShellCommand {
            command: "git status".to_string(),
        }));
    }

    #[test]
    fn control_plane_requests_need_the_driver_reconcile() {
        // The TUI optimistically paints "queued" for these; none of them emit
        // a terminal lifecycle event of their own, so the driver must.
        assert!(!round_owned_request(&AgentRequest::SlashCommand(
            "/yolo on".to_string()
        )));
        assert!(!round_owned_request(&AgentRequest::Interrupt));
        assert!(!round_owned_request(&AgentRequest::SwitchProvider {
            provider_type: "openai".to_string(),
            model: "gpt".to_string(),
            api_key: None,
            base_url: None,
        }));
        assert!(!round_owned_request(&AgentRequest::ToggleTool {
            name: "execute_command".to_string(),
            enabled: false,
        }));
        assert!(!round_owned_request(&AgentRequest::ToggleMcpServer {
            name: "github".to_string(),
            enabled: true,
        }));
        assert!(!round_owned_request(&AgentRequest::RefreshProviderModels {
            user_initiated: true,
        }));
        assert!(!round_owned_request(&AgentRequest::QuerySessionContext));
        assert!(!round_owned_request(&AgentRequest::PermissionReply {
            request_id: "r".to_string(),
            decision: muta_contracts::PermissionDecision::Always,
            parent_call_id: None,
        }));
        assert!(!round_owned_request(&AgentRequest::UserQuestionReply {
            request_id: "r".to_string(),
            answers: Vec::new(),
            parent_call_id: None,
        }));
        assert!(!round_owned_request(&AgentRequest::InputReply {
            request_id: "r".to_string(),
            text: "y".to_string(),
            parent_call_id: None,
        }));
        assert!(!round_owned_request(&AgentRequest::ExitSideView));
        assert!(!round_owned_request(&AgentRequest::FocusSide {
            side_id: "s".to_string()
        }));
        assert!(!round_owned_request(&AgentRequest::InterruptSide {
            side_id: "s".to_string()
        }));
        assert!(!round_owned_request(&AgentRequest::CloseSide {
            side_id: "s".to_string()
        }));
        assert!(!round_owned_request(&AgentRequest::QueryBtwList));
        assert!(!round_owned_request(&AgentRequest::UpdateTuiLayout(
            "default".to_string()
        )));
        assert!(!round_owned_request(&AgentRequest::DeleteSession {
            id: "s".to_string(),
        }));
        assert!(!round_owned_request(&AgentRequest::RenameSession {
            id: "s".to_string(),
            title: Some("t".to_string()),
        }));
        assert!(!round_owned_request(&AgentRequest::QuerySessionDetail {
            id: "s".to_string(),
        }));
    }

    #[tokio::test]
    async fn activity_reconcile_fires_only_for_control_plane_requests_with_no_live_round() {
        let lifecycle = Arc::new(RoundLifecycle::new());
        let yolo = AgentRequest::SlashCommand("/yolo on".to_string());

        // Idle harness + control-plane request → the driver must reconcile.
        assert!(
            needs_activity_reconcile(&yolo, &lifecycle).await,
            "idle + slash command needs the reconcile"
        );

        // Round-owned requests never need the reconcile — the round task emits
        // its own terminal idle snapshot.
        assert!(
            !needs_activity_reconcile(
                &AgentRequest::Chat {
                    text: "hi".to_string(),
                    images: Vec::new(),
                    sent_at_ms: None,
                },
                &lifecycle,
            )
            .await,
            "chat closes its own lifecycle"
        );
        assert!(
            !needs_activity_reconcile(
                &AgentRequest::ShellCommand {
                    command: "git status".to_string(),
                },
                &lifecycle,
            )
            .await,
            "shell closes its own lifecycle"
        );

        // A live round owns the display: even a control-plane request is left
        // alone so the round's timer/turn counters are not reset.
        let begin = lifecycle.begin().await;
        assert!(
            !needs_activity_reconcile(&yolo, &lifecycle).await,
            "live round suppresses the reconcile"
        );
        assert!(lifecycle.finish(begin.generation).await);

        // Back to idle → the reconcile is armed again.
        assert!(
            needs_activity_reconcile(&yolo, &lifecycle).await,
            "idle again → reconcile re-arms"
        );
    }

    fn usage_record(
        session_id: &str,
        actor: &str,
        round: u64,
        turn: u32,
        status: muta_contracts::RequestUsageStatus,
    ) -> muta_contracts::RequestUsageRecord {
        muta_contracts::RequestUsageRecord {
            key: muta_contracts::RequestUsageKey {
                session_id: session_id.to_string(),
                actor_id: actor.to_string(),
                round,
                turn,
                attempt: 1,
            },
            provider: "relay".to_string(),
            model: "m".to_string(),
            status,
            ..Default::default()
        }
    }

    fn residue_store(directory: &std::path::Path) -> Arc<SessionStore> {
        std::fs::create_dir_all(directory).expect("create test directory");
        Arc::new(SessionStore::for_path(directory.join("session.json")))
    }

    #[tokio::test]
    async fn crash_residue_arms_retry_point_for_the_in_flight_round() {
        // The scenario from the wild: the process died mid-round, so no
        // graceful path armed `/retry` and the resumed session answered
        // "Nothing to retry". Recovery must arm it from the durable
        // `InFlight` usage record.
        let directory =
            std::env::temp_dir().join(format!("muta-crash-retry-{}", uuid::Uuid::new_v4()));
        let store = residue_store(&directory);
        let session_id = store.id().await;
        // Round 2 in flight (turn 2 = the second ReAct turn, the one that
        // died), one committed assistant turn from turn 1, a settled round 1.
        // The transcript carries no round delimiters: the crashed round's
        // opener is the *last* visible user message, so round 1's assistant
        // reply must not be counted into round 2.
        store
            .set_request_usage_records(vec![
                usage_record(&session_id, "master", 1, 1, RequestUsageStatus::Completed),
                usage_record(&session_id, "master", 2, 1, RequestUsageStatus::Completed),
                usage_record(&session_id, "master", 2, 2, RequestUsageStatus::InFlight),
                usage_record(&session_id, "runner:c1", 2, 5, RequestUsageStatus::InFlight),
            ])
            .await
            .unwrap();
        store
            .replace_messages(vec![
                Message::new(Role::User, "round 1 prompt"),
                Message::new(Role::Assistant, "round 1 answer"),
                Message::new(Role::User, "round 2 prompt"),
                Message::new(Role::Assistant, "round 2 turn 1"),
                Message::new(Role::Tool, "ok"),
            ])
            .await
            .unwrap();
        store.set_round_counter(2).await.unwrap();

        let residue = recover_crashed_round(
            &store,
            store.request_usage_records().await,
            1_700_000_000_000,
        )
        .await;

        // Interrupt for the dangling round, so the transcript explains it.
        assert_eq!(residue.interrupts.len(), 1);
        assert_eq!(residue.interrupts[0].round, Some(2));
        assert_eq!(
            residue.interrupts[0].reason,
            muta_contracts::RoundInterruptReason::Terminated
        );
        // The point names the highest in-flight round (not the runner's key),
        // counts only committed master turns, and watermarks the durable
        // window.
        let point = residue
            .retry_point
            .expect("crash residue arms a retry point");
        assert_eq!(point.round, 2);
        assert_eq!(point.turns_committed, 1, "one committed ReAct turn");
        assert_eq!(point.history_watermark, 5);
        assert_eq!(point.at_ms, 1_700_000_000_000);

        let _ = std::fs::remove_dir_all(directory);
    }

    #[tokio::test]
    async fn crash_residue_ignores_settled_sessions_and_lower_in_flight_rounds() {
        let directory =
            std::env::temp_dir().join(format!("muta-crash-settled-{}", uuid::Uuid::new_v4()));
        let store = residue_store(&directory);
        // Nothing in flight → no residue at all.
        let empty = recover_crashed_round(&store, Vec::new(), 1).await;
        assert!(empty.interrupts.is_empty() && empty.retry_point.is_none());

        let session_id = store.id().await;
        store
            .set_request_usage_records(vec![
                usage_record(&session_id, "master", 1, 1, RequestUsageStatus::Completed),
                // Round 2 still in flight — but round 3 has since completed,
                // so 2 is history: the counter guard must retire it.
                usage_record(&session_id, "master", 2, 1, RequestUsageStatus::InFlight),
                usage_record(&session_id, "master", 3, 1, RequestUsageStatus::Completed),
            ])
            .await
            .unwrap();
        store.set_round_counter(3).await.unwrap();
        let superseded =
            recover_crashed_round(&store, store.request_usage_records().await, 1).await;
        assert!(superseded.retry_point.is_none(), "superseded round retired");
        // The dangling-round interrupt is still recorded for the transcript.
        assert_eq!(superseded.interrupts.len(), 1);
        assert_eq!(superseded.interrupts[0].round, Some(2));

        let _ = std::fs::remove_dir_all(directory);
    }

    #[tokio::test]
    async fn crash_residue_arms_even_over_an_existing_terminated_interrupt() {
        // Two scenarios land here. (a) A graceful daemon kill: the registry
        // records `Terminated` but the round is exactly as resumable as a
        // crash's — suppressing the point would resurrect the bug for every
        // `muta stop`. (b) A crash *during* a `/retry` resume: the resumed
        // round keeps its number, so the earlier run's `Terminated` record is
        // already present when the second crash is recovered. The interrupt
        // explains the transcript; the point offers recovery — independent.
        let directory =
            std::env::temp_dir().join(format!("muta-crash-graceful-{}", uuid::Uuid::new_v4()));
        let store = residue_store(&directory);
        let session_id = store.id().await;
        store
            .set_request_usage_records(vec![usage_record(
                &session_id,
                "master",
                1,
                1,
                RequestUsageStatus::InFlight,
            )])
            .await
            .unwrap();
        store
            .record_round_interrupt(muta_contracts::RoundInterrupt {
                reason: muta_contracts::RoundInterruptReason::Terminated,
                at_ms: 1,
                round: Some(1),
            })
            .await
            .unwrap();
        store.set_round_counter(1).await.unwrap();
        let residue = recover_crashed_round(&store, store.request_usage_records().await, 2).await;
        // The record already exists — `record_round_interrupt` dedupes on
        // (reason, round) — but the resume point is armed regardless.
        assert_eq!(residue.interrupts.len(), 1);
        assert!(
            residue.retry_point.is_some(),
            "an existing interrupt must not suppress the resume point"
        );

        let _ = std::fs::remove_dir_all(directory);
    }

    #[tokio::test]
    async fn crash_residue_counts_turns_from_the_rounds_opening_user_message() {
        // A crash on the round's very first request: the user message is
        // durable, nothing streamed through yet → zero committed turns. A
        // prior round's assistant reply must not be counted into it.
        let directory =
            std::env::temp_dir().join(format!("muta-crash-first-{}", uuid::Uuid::new_v4()));
        let store = residue_store(&directory);
        let session_id = store.id().await;
        store
            .set_request_usage_records(vec![usage_record(
                &session_id,
                "master",
                2,
                1,
                RequestUsageStatus::InFlight,
            )])
            .await
            .unwrap();
        store
            .replace_messages(vec![
                Message::new(Role::User, "round 1"),
                Message::new(Role::Assistant, "answer 1"),
                Message::new(Role::User, "round 2"),
            ])
            .await
            .unwrap();
        store.set_round_counter(2).await.unwrap();
        let residue = recover_crashed_round(&store, store.request_usage_records().await, 1).await;
        let point = residue
            .retry_point
            .expect("first-request crash arms a point");
        assert_eq!(point.turns_committed, 0, "nothing streamed through");
        assert_eq!(point.history_watermark, 3);

        let _ = std::fs::remove_dir_all(directory);
    }
}
