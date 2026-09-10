//! The shared session-harness factory for every frontend binary (ADR-0037
//! Step 6).
//!
//! [`assemble`] performs the full session startup that used to live inline in
//! the `muta` binary's `main`: channel creation, custom-command discovery,
//! config load + migrations, background live model discovery, store opens,
//! the repeat scheduler, provider/skills/toolset wiring, `SubagentTool` layering,
//! agent construction, MCP background connect, pursuit/todo/session-state
//! restore, and finally [`SessionDriver`] construction — in the exact order
//! the original `main` did, with the same background spawns.
//!
//! The crate stays application-neutral (ADR-0054): the caller supplies the
//! [`AgentIdentity`], the [`AgentPreset`], and the [`UiBridge`] as
//! parameters. Nothing here names a product.
//!
//! `SessionStart::Version`, `SessionStart::Doctor`, `SessionStart::Attach`, and
//! `SessionStart::Showcase` are **not** handled here: they are purely local
//! (or client-side) short-circuits and must be dispatched by the caller
//! before invoking [`assemble`].

use muta_agent::catalog;
use muta_agent::orchestration::{MidTurnPruneProjectionGate, ProxyProvider, round_response};
use muta_agent::{Agent, AgentIdentity, AgentPreset, RoundLifecycle, SubagentTool};
use muta_contracts::{
    AgentNotice, AgentRequest, AgentResponse, Message, NoticeKind, NoticeSeverity, NoticeSource,
    NoticeSurface, Provider, RoundEvent, SUBAGENT_EXPLORE, ToolContextBuilder, ToolSet,
    WorkspaceTrustState, collect_toolset,
};

use muta_mcp::{McpCatalog, McpRuntime};
use muta_persistence::{
    config::Config, connection_usage, paths, session::SessionStore,
    workspace_security::WorkspaceSecurityStore,
};
use muta_skills::SkillRegistry;

use crate::startup::SessionStart;
use crate::{SessionDriver, UiBridge};

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::RwLock;
use tokio::sync::{RwLock as AsyncRwLock, mpsc};

/// Everything a frontend binary must supply to assemble a session harness.
///
/// The identity and master are the *only* application-specific inputs; all
/// other behavior is shared across frontends.
pub struct BootstrapParams {
    /// The agent's identity (name + mission), bound at construction.
    pub identity: AgentIdentity,
    /// The declarative agent preset profile (ADR-0053), applied after
    /// construction and before the `[agent]` config overlay.
    pub preset: AgentPreset,
    /// The frontend's clipboard/UI bridge (used by `/export`).
    pub ui: Arc<dyn UiBridge>,
    /// How the session begins (ADR-0116: only the assembly-relevant
    /// shapes exist here; one-shot CLI modes never reach the harness).
    pub startup: SessionStart,
    /// `--project` override; when `None`, the current directory is used.
    pub project_root: Option<PathBuf>,
    /// `--unattended` at start (unattended execution): auto-approve tool permissions.
    pub unattended: bool,
    /// Workspace filesystem confinement (default true). False (`--no-confinement`) bypasses confinement.
    pub confined: bool,
    /// ADR-0141: the human-channel accountant this session reports into.
    /// Attach/detach on the WS layer ORs client postures into it; the
    /// harness's posture gate reads it before parking a human request.
    /// `None` on one-shot CLI paths that never attach.
    pub human_channel: Option<Arc<muta_contracts::human_request::HumanChannelAccountant>>,
    /// Session-lifetime cancellation token (ADR-0125): passed through to the
    /// background `/schedule` scheduler so it stops when the harness is torn
    /// down (suspension, kill, daemon drain) instead of ticking forever.
    /// `None` = process-lifetime scheduling (single-session frontends).
    pub teardown_token: Option<tokio_util::sync::CancellationToken>,
    /// ADR-0209: Daemon-wide authoritative configuration shared across all hosted sessions.
    pub shared_config: Option<crate::SharedConfig>,
    /// ADR-0209: Daemon-wide authoritative connection usage shared across all hosted sessions.
    pub shared_provider_usage: Option<crate::SharedConnectionUsage>,
}

/// The assembled session harness: the driver (ready to `run`), the frontend
/// ends of the request/response channels, and the values the frontend needs
/// to start its UI and wind the session down.
pub struct Bootstrap {
    /// The session driver, fully wired. The caller moves it into a task
    /// (`tokio::spawn(driver.run())`).
    pub driver: SessionDriver,
    /// The frontend's request sender (the driver holds the receiver).
    pub req_tx: mpsc::Sender<AgentRequest>,
    /// The frontend's response receiver (the driver holds the sender).
    pub resp_rx: mpsc::UnboundedReceiver<AgentResponse>,
    /// An `Arc` handle on the primary agent so the caller can fire
    /// SessionEnd hooks (ADR-0025) after its UI returns — the driver task
    /// owns the agent by then.
    pub agent_for_session_end: Arc<Agent>,
    /// The primary session store, shared with the driver.
    pub session: Arc<SessionStore>,
    /// Shared token-source ledger, shared with the driver; the frontend reads
    /// it for the token-source report.
    pub token_ledger: Arc<muta_contracts::TokenSourceLedger>,
    /// The provider name the UI should display at startup.
    pub initial_provider_name: String,
    /// The model name the UI should display at startup.
    pub initial_model_name: String,
    /// The session's restored transcript (empty for a fresh session).
    pub restored_messages: Vec<Message>,
    /// Complete daemon-owned command/completion vocabulary for this session.
    pub command_catalog: muta_contracts::CommandCatalog,
    /// The primary agent (same `Arc` as `agent_for_session_end`), exposed so
    /// the registry can publish session-scoped tools onto it.
    pub agent: Arc<Agent>,
    /// The workspace-authority store assembled for this session's project
    /// root. The registry surfaces it on [`crate::registry::BoundSession`]
    /// so the WS attach path can detect an unconfigured workspace and push
    /// the trust decision to the attaching client. Bootstrap itself no
    /// longer emits the trust question over `resp_rx` (see `assemble`): the
    /// broadcast channel does not exist yet at that point, so the event
    /// reached nobody. Attach-time replay in `serve.rs` is the delivery
    /// mechanism instead.
    pub security: Arc<WorkspaceSecurityStore>,
    /// Live additional-roots handle sharing state with the execution
    /// environment. `/trust` grant/revoke and `/settings reload` recompute
    /// the admitted set through it — effective on the next tool call.
    pub shared_additional_roots: muta_contracts::SharedAdditionalRoots,
    /// Live handle for toggling session-level workspace confinement.
    pub shared_confinement: muta_contracts::SharedConfinement,
}

/// Ensure the four XDG application roots exist. Best-effort.
pub fn ensure_app_roots() {
    let dirs = paths::get();
    for dir in [
        &dirs.config_dir,
        &dirs.data_dir,
        &dirs.state_dir,
        &dirs.cache_dir,
    ] {
        if let Err(error) = std::fs::create_dir_all(dir) {
            tracing::warn!(?error, dir = %dir.display(), "bootstrap: could not create app dir");
        }
    }
}

/// Assemble one live session harness. See the module docs for the contract.
///
/// The ordering and background-spawn behavior are identical to the original
/// inline `main`: live model discovery, skill catalog
/// refresh, MCP connect/refresh, and the schedule scheduler (which holds a
/// `req_tx` clone) all run in the background so they never delay the first
/// frame.
#[allow(clippy::too_many_lines)]
pub async fn assemble(params: BootstrapParams) -> Result<Bootstrap, Box<dyn std::error::Error>> {
    let BootstrapParams {
        identity,
        preset,
        ui,
        startup,
        project_root: project_override,
        unattended: unattended_at_start,
        confined: confined_at_start,
        human_channel,
        teardown_token: _,
        shared_config,
        shared_provider_usage,
    } = params;
    debug_assert!(
        matches!(
            startup,
            SessionStart::Fresh
                | SessionStart::FreshWithPrompt(_)
                | SessionStart::Resume(_)
                | SessionStart::Picker
        ),
        "assemble only handles Fresh/FreshWithPrompt/Resume/Picker; other modes must short-circuit in the caller"
    );

    // First-run friendliness: this harness opens stores eagerly (the session
    // store and embedding index under data_dir) and does not create their
    // parent dirs first — on a developer's machine those dirs usually already
    // exist from prior runs, but any binary may be started into a fresh XDG
    // root (wrappers, CI, sandboxes, a spawned session server). Create the
    // four app roots up front, BEFORE any store opens; best-effort,
    // everything deeper stays lazy as in production.
    {
        let dirs = paths::get();
        for dir in [
            &dirs.config_dir,
            &dirs.data_dir,
            &dirs.state_dir,
            &dirs.cache_dir,
        ] {
            if let Err(error) = std::fs::create_dir_all(dir) {
                tracing::warn!(?error, dir = %dir.display(), "bootstrap: could not create app dir");
            }
        }
    }

    /// Bound capacity for inbound session requests to prevent unbounded memory growth.
    const SESSION_REQUEST_CAPACITY: usize = 512;
    let (req_tx, req_rx) = mpsc::channel::<AgentRequest>(SESSION_REQUEST_CAPACITY);
    let (resp_tx, resp_rx) = mpsc::unbounded_channel::<AgentResponse>();

    let shared_config =
        shared_config.unwrap_or_else(|| Arc::new(tokio::sync::RwLock::new(Config::load())));
    let shared_provider_usage = shared_provider_usage.unwrap_or_else(|| {
        Arc::new(tokio::sync::RwLock::new(
            connection_usage::ConnectionUsage::load(),
        ))
    });

    let mut config = shared_config.read().await.clone();
    // Overlay persisted fitted-model metadata onto model resolution, so ids a
    // trusted provider advertised (but the static registry does not know)
    // resolve with their real capabilities from the very first request.
    catalog::sync_fitted_model_registry();

    // Live model-list discovery for API-sourced instances. Runs in the
    // BACKGROUND so slow/unreachable providers never delay the first frame:
    // every instance already has either its fixed snapshot or last known valid
    // subset. The live `GET /models` result feeds the catalog's remote-catalog
    // overlay (ADR-0203); transport/status/schema failure leaves that subset
    // untouched, while a valid empty catalog clears it. The session driver
    // handles the refresh and broadcasts updated snapshots to the client.
    let req_tx_for_discovery = req_tx.clone();
    tokio::spawn(async move {
        let _ = req_tx_for_discovery
            .send(AgentRequest::RefreshProviderModels {
                user_initiated: false,
            })
            .await;
    });

    // Background refresh of the models.dev third-party catalog cache
    // (LiveCatalog::ModelsDev — opencode-go). This only keeps the *disk cache*
    // fresh on an hourly cadence; the per-connection reconciliation runs when
    // discovery next fires (startup, `/refresh`, per-round ETag) and reads the
    // refreshed cache. A failure leaves the last good cache in place, so a
    // transient outage never degrades the catalog.
    muta_agent::dynamic::spawn_refresh(muta_models_dev::DynamicModelsDev);

    // Resolve the project root early: it feeds the per-project lock, the
    // session store, and the embedding index. CLI parsing happened in the
    // caller (showcase/doctor already returned there).
    let project_root = project_override.clone().unwrap_or_else(|| {
        std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
    });

    // Initialize Agent logic. The provider is resolved through the model
    // catalog (`build_provider_for`), the single source of truth for the
    // env-var-then-config resolution rules shared with runtime switching. See
    // `docs/adr/0002-model-channel-abstraction.md`.

    // ADR-0116: the pre-0018 per-project exclusive lock is gone — the
    // unified daemon owns every session and the CLI flag was dead (parsed,
    // discarded). Sessions still pin their own `sessions/<id>.{json,jsonl}`
    // (ADR-0018), so concurrency is safe without a project-wide lock.

    // Session loading honors the startup mode. Under ADR-0018
    // `load_for_project` pins a fresh `sessions/<id>.{json,jsonl}`, so a bare
    // start always begins a new session; prior sessions stay on disk and are
    // reachable through the picker or `attach`. `mutx attach <id>` opens
    // that exact session — a missing target is a hard error (propagated via
    // `?`) rather than a silent fresh-session fallback, so the operator knows
    // the attach never happened. `mutx attach` (no id) opens the sessions
    // picker overlay instead of guessing.
    let session = Arc::new(SessionStore::load_for_project(project_root.clone()));
    let open_picker_on_start = match &startup {
        SessionStart::Fresh | SessionStart::FreshWithPrompt(_) => false,
        SessionStart::Picker => true,
        SessionStart::Resume(id) => {
            session.resume(Some(id.as_str())).await?;
            false
        }
    };

    // Background `/schedule` scheduler, bound to THIS session. Every 30s it prunes
    // expired jobs and fires any that are due, dispatching each prompt as a
    // normal `AgentRequest::Chat` round. Drives both recurring cron jobs and
    // one-shot (countdown / absolute-time) jobs. Jobs are session-scoped state
    // now, so a resumed session's schedule is already loaded above and the
    // scheduler runs against it from the first tick. Supervised: a panic in
    // the tick loop restarts with backoff instead of silently killing every
    // scheduled job in the session.
    //
    // Teardown token (ADR-0125): the registry passes the hosted session's
    // cancellation token, so suspension/kill stops the tick with the harness.
    // Before this the task leaked past teardown and ticked against a dead
    // channel forever. `None` (a plain process-lifetime scheduler) remains
    // available for single-session frontends that tear down with the process.
    // C6: overlay the session's provider/model pin onto the effective config
    // before building the initial provider. A session that previously ran
    // `/models` reopens on its own provider instead of the global default,
    // so one session's choice never bleeds into another. Done after the session
    // is loaded (and, for resume, after `resume` swapped in its data).
    if let Some(selection) = session.provider_selection().await {
        config.default_connection = selection.connection.clone();
        if let Some(model) = selection.model {
            config.default_model = Some(model);
        }
    }

    // The catalog returns `None` when no real channel resolves (empty config or
    // an unknown default). Install the explicit `NoProvider` sentinel so the
    // holder type is satisfied; the chat dispatch refuses up-front with a
    // user-facing notification while this sentinel is live.
    let provider_id = catalog::default_provider_id(&config);
    crate::handlers_provider::refresh_oauth_if_needed(&config, provider_id).await;

    let session_id = session.id().await;
    let initial_provider: Arc<dyn Provider> = catalog::build_provider_for_model(
        &config,
        provider_id,
        config.default_model.as_deref(),
        Some(&session_id),
    )
    .or_else(|| catalog::build_provider_for(&config, provider_id))
    .unwrap_or_else(|| Arc::new(muta_agent::NoProvider));

    let provider_holder = Arc::new(RwLock::new(initial_provider));
    let provider_for_task = provider_holder.clone();

    let agent_provider = Arc::new(ProxyProvider::new(provider_holder));

    // Shared skills registry for the skill tools. The registry starts EMPTY so
    // discovering skills (scanning local dirs, cloning/fetching remote repos)
    // never blocks the first frame; the background refresh loop re-scans all
    // sources immediately on spawn and then every hour. The `Arc` is shared
    // across the skill tools, the subagent profile, and the frontend, so once the
    // background load lands they all observe the populated state.
    //
    // Pin the session's project root into the skills config so the
    // project-local sources (`.muta/skills` etc.) resolve from this
    // session's project — not the daemon process's cwd, which under the
    // unified daemon (ADR-0096) belongs to whichever client first spawned it.
    let mut skills_config = config.skills.clone();
    skills_config.project_root = Some(project_root.clone());
    let skills_registry = Arc::new(SkillRegistry::empty_with_config(&skills_config));
    // A content-admitted `.muta/skills/<name>/SKILL.md` wins over a same-named
    // user or remote skill by priority. Surface every newly observed shadow so
    // that prompt injection cannot hide behind normal precedence. Install the
    // sink before background refresh so startup, `/skills reload`, and
    // `/trust` reports through the same path.
    {
        let resp_tx_for_shadows = resp_tx.clone();
        let session_id_for_shadows = session.id().await;
        skills_registry.set_shadow_sink(Some(Arc::new(move |shadowed| {
            for shadow in shadowed {
                let _ = resp_tx_for_shadows.send(round_response(
                    &session_id_for_shadows,
                    RoundEvent::Notice(
                        AgentNotice::trust_changed(format!(
                            "Project skill '{}' overrides the {}-scope skill of the same name",
                            shadow.name, shadow.overridden_scope
                        ))
                        .with_body(format!(
                            "Loading {} instead. Project-local skills win by priority; \
                             if this is unexpected, inspect the project's skills directories \
                             (.muta/skills, skills) or run \
                             `/untrust`.",
                            shadow.winner_source.display()
                        )),
                    ),
                ));
            }
        })));
    }
    skills_registry.spawn_reactive_watcher();

    // Built-in tools self-register via `inventory` (most tools carry a
    // `register_tool!` submission at its definition site) and are collected
    // here from a single opaque context. Tools that need runtime state (the web
    // tools' search config, the shared skill registry, the embedding index +
    // session store) pull it out of the context by type — see
    // `muta_contracts::tool_registry`. Stateful/meta tools that genuinely depend on the
    // *rest* of the toolset (the subagent dispatch `task`) cannot
    // self-register and are assembled explicitly below. MCP tools are
    // discovered at runtime and published directly to the master Agent;
    // they are not part of this static capability set.
    // Spatial admission resolves global and trusted project-declared roots.
    let workspace_security = Arc::new(WorkspaceSecurityStore::load());
    let security_snapshot = workspace_security.snapshot(&project_root);
    if security_snapshot.ex_workspace.is_trusted() {
        config.merge_project_additional_roots(Config::load_project_additional_roots(&project_root));
    }
    let resolved_additional = config
        .resolve_workspace_additional_roots_detailed(&project_root)
        .unwrap_or_default();
    for (raw, reason) in &resolved_additional.skipped {
        tracing::warn!(
            root = %raw,
            %reason,
            "additional workspace root skipped"
        );
    }
    let additional_roots: Vec<std::path::PathBuf> = resolved_additional.admitted;
    // Hot-updatable `[web]` handle: the web tools receive the same handle via
    // the tool context, and `UpdateWebSearchConfig` atomically replaces its
    // resolved snapshot. Changes reach the next call without a toolset rebuild.
    let resolved_web = muta_persistence::config::resolve_web_config(
        &config.web,
        &muta_persistence::config::Credentials::load(),
    );
    let websearch_shared = muta_contracts::SharedWebConfig::new(resolved_web.runtime);
    let execution_env = Arc::new(
        muta_agent::execution::WorkspaceExecutionEnvironment::with_additional_roots(
            project_root.clone(),
            additional_roots.clone(),
        ),
    );
    let shared_additional_roots = execution_env.shared_additional_roots();
    let shared_confinement = execution_env.shared_confinement();
    let background_jobs = crate::background_jobs::BackgroundJobManager::new();
    let session_job_service = crate::background_jobs::SessionJobService::new(
        background_jobs.clone(),
        execution_env.clone() as Arc<dyn muta_contracts::ExecutionEnvironment>,
    );
    // ADR-0190 D5: bind the owning session so every task this service
    // spawns is stamped with it (snapshots + ledger rows).
    session_job_service.bind_owner(session.id().await);
    let job_service: Arc<dyn muta_contracts::BackgroundJobService> = Arc::new(session_job_service);

    let tool_ctx = {
        let mut builder = ToolContextBuilder::new();
        builder.provide(websearch_shared.clone());
        builder.provide(websearch_shared.get());
        builder.provide(skills_registry.clone());
        builder.provide(session.clone());
        builder.provide(execution_env.clone() as Arc<dyn muta_contracts::ExecutionEnvironment>);
        builder.provide(job_service);
        // The session's workspace root: every workspace-relative tool
        // operation (bash cwd, relative path resolution, search bases)
        // anchors here instead of the daemon process's cwd. Under the
        // unified daemon (ADR-0096) one process hosts sessions for many
        // projects, so the process cwd is whichever directory the first
        // client spawned it from — correct only by coincidence. This is the
        // fix for "launched in project A, session edits project B".
        builder.provide(muta_contracts::WorkspaceRoot(project_root.clone()));
        builder.provide(muta_contracts::WorkspaceRoots::new(
            project_root.clone(),
            additional_roots.clone(),
        ));
        builder.provide(shared_additional_roots.clone());
        builder.provide(shared_confinement.clone());
        builder.build()
    };
    let mut toolset: ToolSet = collect_toolset(&tool_ctx);
    // MCP tools are discovered after Agent construction and published through
    // its connector-neutral dynamic-tool sink. The MCP runtime owns protocol
    // and connection state; the agent owns advertisement and dispatch.
    // Snapshot of the shared toolset (built-in default variants) before the
    // `SubagentTool` is layered on. A `/btw` side session (ADR-0017) rebuilds
    // its `Agent` from this same snapshot — minus its own `SubagentTool` and
    // without inheriting the master's session-scoped connector sources.
    let base_tools: Arc<Vec<Arc<dyn muta_contracts::Tool>>> = Arc::new(toolset.default_view());
    // SubagentTool gets the static capability set (excluding itself) so spawned
    // subagents cannot recurse and inherit the live provider. Dynamic connector
    // sources are master-only unless a future policy explicitly delegates
    // them. It binds the SUBAGENT_EXPLORE profile (read-only / non-interactive /
    // non-recursive).
    let subagent_tool = Arc::new(SubagentTool::new(
        agent_provider.clone(),
        toolset.clone(),
        &SUBAGENT_EXPLORE,
    ));
    // Subagents resolve relative write-grants against the session's project
    // root, not the daemon process's cwd (ADR-0096).
    subagent_tool.set_workspace_root(Some(project_root.clone()));
    // Subagents inherit the session's connection retry configuration.
    subagent_tool.bind_retry_policy(
        config.connection_retry_max_attempts,
        config.connection_retry_base_ms,
        config.connection_retry_max_ms,
    );
    // Full-duplex (ADR-0029): capture the subagent tool's subagent registry so the
    // request loop can route a user's permission / ask_user reply down into the
    // specific live child that surfaced the request (looked up by the parent
    // tool-call id the frontend tags onto the reply). Captured before
    // `subagent_tool` is layered into the capability set.
    let subagent_registry = subagent_tool.registry();
    // Keep a typed handle so we can bind the parent's variant selection into the
    // subagent tool once the agent (which owns that selection) exists. The same
    // underlying `Arc<SubagentTool>` is what gets layered into the toolset.
    let subagent_tool_handle = subagent_tool.clone();
    toolset.insert(subagent_tool);
    let mut agent = Agent::builder_from_toolset(agent_provider, toolset, identity)
        .with_skills((*skills_registry).clone())
        .build();
    // Surface the admitted additional roots to the model (ADR-0142): the
    // system prompt tells it cross-project paths are legal instead of letting
    // it discover the widened boundary through trial and error.
    agent.set_additional_workspace_roots(additional_roots.clone());
    agent.bind_shared_confinement(shared_confinement.clone());
    let agent = Arc::new(agent);
    // Override axis (model): subagents are agents on the same model, so they
    // inherit the parent's tool-variant selection. The profile still owns the
    // orthogonal scope axis.
    subagent_tool_handle.bind_variant_selection(agent.variant_selection_handle());
    subagent_tool_handle.bind_workspace_security(agent.workspace_security_handle());
    subagent_tool_handle.bind_execution_policy(agent.execution_policy());
    // ADR-0138 §2: expose the master's live dynamic (MCP) tool registry to
    // subagent dispatch. The mcp_specialist child resolves its toolset from this
    // source at spawn time, so McpCatalog re-discovery reaches later children
    // without re-binding. Other profiles are unaffected — only mcp_specialist
    // consults the source.
    subagent_tool_handle.bind_dynamic_tool_source(agent.dynamic_tool_source());
    // ADR-0141: subagents inherit the session's live human channel.
    if let Some(accountant) = human_channel.as_ref() {
        subagent_tool_handle.bind_human_channel(Arc::clone(accountant));
    }
    // Wire the per-project "always allow" allowlist so prior `Always`
    // approvals survive across sessions in this project. Best-effort: a
    // missing or unreadable permissions.json just means we re-prompt.
    agent.set_project_root(Some(project_root.clone()));
    // Seed declarative permission rules from `[permissions]` config so default
    // policies are data-driven. Runtime "Always" decisions still write to
    // permissions.json; these config rules re-apply on every start.
    agent.seed_permissions_from_config(&config.permissions.allow);
    // Asset trust, filesystem boundaries, and runtime execution grants are
    // independent axes. Opening a path grants none of them.
    // `.muta/config.toml` may declare `[mcp.*]` servers (which execute
    // processes) and `[[hooks]]` entries (which run shell commands at lifecycle
    // points); its `.muta/skills` and `.muta/commands` trees inject
    // project-authored prompt text (skills can also shadow the user's own
    // same-named skills by priority). Loading those automatically from a
    // cloned or vendored working tree is the same class of hazard as an npm
    // `postinstall` script or a git hook: a malicious repo must not gain code
    // execution — or prompt injection, which for an agent holding tools is
    // execution-by-proxy — merely because the user opened it. Every concrete
    // domain loads only after its own exact content has been
    // explicitly trusted. Global config is user-authored and trusted
    // unconditionally.
    agent.set_workspace_security(security_snapshot.clone());
    let project_mcp = Config::load_project_mcp(&project_root);
    let project_hooks = Config::load_project_hooks(&project_root);
    if security_snapshot.mcp.is_trusted() && !project_mcp.is_empty() {
        config.merge_project_mcp(project_mcp);
    }
    if security_snapshot.hooks.is_trusted() && !project_hooks.is_empty() {
        config.merge_project_hooks(project_hooks);
    }
    if security_snapshot.instructions.is_trusted() {
        match crate::project::load_project_rules(&project_root) {
            Ok(rules) => agent.set_project_rules(rules),
            Err(error) => tracing::warn!(%error, "trusted project rules could not be loaded"),
        }
    }
    let gated = [
        ("mcp", security_snapshot.mcp),
        ("skills", security_snapshot.skills),
        ("hooks", security_snapshot.hooks),
        ("instructions", security_snapshot.instructions),
        ("ex-workspace", security_snapshot.ex_workspace),
    ]
    .into_iter()
    .filter_map(|(domain, state)| {
        matches!(
            state,
            WorkspaceTrustState::Quarantined | WorkspaceTrustState::Changed
        )
        .then_some(format!("{domain} ({})", state.as_str()))
    })
    .collect::<Vec<_>>();
    if !gated.is_empty() {
        let _ = resp_tx.send(round_response(
            &session.id().await,
            RoundEvent::Notice(
                AgentNotice::trust_changed("Project assets are quarantined")
                    .with_surface(NoticeSurface::Banner)
                    .with_body(format!(
                        "Quarantined domains: {}. Inspect them, then run `/trust` or `/trust <domain>` (e.g. `/trust instructions`).",
                        gated.join(", ")
                    )),
            ),
        ));
    }
    if !resolved_additional.skipped.is_empty() {
        let details = resolved_additional
            .skipped
            .iter()
            .map(|(p, r)| format!("`{p}` ({r})"))
            .collect::<Vec<_>>()
            .join(", ");
        let _ = resp_tx.send(round_response(
            &session.id().await,
            RoundEvent::Notice(
                AgentNotice::new(
                    NoticeKind::ReviewAlert,
                    NoticeSeverity::Warning,
                    "Some additional workspace roots could not be loaded",
                    NoticeSource::Harness,
                )
                .with_surface(NoticeSurface::Inline)
                .with_body(format!("Skipped roots: {details}")),
            ),
        ));
    }
    let command_catalog = crate::startup::command_catalog(&[]);
    // Wire workspace security trust verifier into muta-mcp so MCP server
    // connections can verify sandbox trust without depending on muta-persistence.
    let ws_security_for_mcp = workspace_security.clone();
    muta_mcp::set_trust_verifier(Arc::new(move |root| {
        ws_security_for_mcp
            .snapshot(root)
            .is_trusted(muta_contracts::TrustDomain::Mcp)
    }));
    // Connect every configured MCP server in the BACKGROUND so a slow/unreachable
    // server (8s connect timeout each) never delays the first frame. The
    // runtime is ready immediately with every enabled server in `Connecting`;
    // a spawned task performs the real concurrent connects and seeds the
    // agent's dynamic tool sink as each comes online. The frontend's status
    // snapshot reflects this transient state, and the periodic McpCatalog
    // refresh keeps it live thereafter.
    let mcp_runtime = Arc::new(McpRuntime::start_background(
        config.mcp.clone(),
        agent.dynamic_tool_sink(),
    ));
    let mcp_runtime_for_bg = Arc::clone(&mcp_runtime);
    tokio::spawn(async move {
        mcp_runtime_for_bg.refresh_all().await;
    });
    muta_agent::dynamic::spawn_refresh(McpCatalog::new(mcp_runtime.clone()));
    if unattended_at_start {
        agent.set_unattended(true);
        if let Err(error) = session.set_unattended(true).await {
            tracing::warn!(
                error = %error,
                "could not persist --unattended startup posture"
            );
        }
        let _ = resp_tx.send(round_response(
            &session.id().await,
            RoundEvent::UnattendedChanged(true),
        ));
        let _ = resp_tx.send(round_response(
            &session.id().await,
            RoundEvent::Notice(
                AgentNotice::new(
                    muta_contracts::NoticeKind::CommandAck,
                    muta_contracts::NoticeSeverity::Info,
                    "Unattended mode ON",
                    muta_contracts::NoticeSource::Harness,
                )
                .with_surface(muta_contracts::NoticeSurface::Toast)
                .with_body(
                    "All tool permissions are auto-approved this session.\n\
                     Use `/unattended off` to return to interactive mode.",
                ),
            ),
        ));
    }
    if !confined_at_start {
        shared_confinement.set_confined(false);
        let _ = resp_tx.send(round_response(
            &session.id().await,
            RoundEvent::ConfinementChanged(false),
        ));
        let _ = resp_tx.send(round_response(
            &session.id().await,
            RoundEvent::Notice(
                AgentNotice::new(
                    muta_contracts::NoticeKind::CommandAck,
                    muta_contracts::NoticeSeverity::Warning,
                    "Workspace Confinement OFF",
                    muta_contracts::NoticeSource::Harness,
                )
                .with_surface(muta_contracts::NoticeSurface::Toast)
                .with_body(
                    "Tools may access and edit any file on the host system.\n\
                     Use `/confinement on` to restore workspace confinement.",
                ),
            ),
        ));
    }

    // ADR-0209: The authoritative usage telemetry lives in shared_provider_usage.
    // Read the current state for startup model resolution without redundant disk I/O.

    // `mutx attach` (no id) opens the sessions picker at startup instead of
    // loading any session: no transcript, todos, or SessionStart hooks should
    // run against the throwaway fresh session — the real session is restored
    // only once the user picks one from the picker (`/session open`). Fresh and
    // `mutx attach <id>` loads eagerly as before.
    let is_picker = matches!(startup, SessionStart::Picker);

    let restored_messages = if is_picker {
        Vec::new()
    } else {
        session.full_transcript().await
    };

    // Mid-turn context projection: when pruning is enabled, install a gate that
    // clears old tool results between ReAct turns once pressure crosses the
    // prune threshold. The threshold is derived from the active model's context
    // window and re-seeded whenever the provider switches (see
    // `reseed_prune_threshold`), so it tracks the live model rather than a
    // fixed token budget.
    if config.compaction.prune {
        agent.set_context_projection_gate(Some(Arc::new(MidTurnPruneProjectionGate {
            session: session.clone(),
            // ADR-0120: token-native — the config key was always tokens; the
            // old ×4 char conversion existed only for the byte-space pruner.
            prune_protect_tokens: config.compaction.prune_protect_tokens,
            // Share the agent's content-addressed weights cache so the gate's
            // post-prune estimate is a cache walk on the blocking pool, not a
            // full-window BPE pass on the async executor.
            weights: agent.token_weights_handle(),
        })));
        crate::agent_setup::reseed_prune_threshold(&agent, &config);
    }

    // Seed per-model tool-variant selection for the startup model. Each listed
    // capability is realized by its chosen variant in the schemas sent to the
    // provider; re-seeded on provider/model switch.
    crate::agent_setup::reseed_tool_variants(&agent, &config);

    // Bind the caller-supplied agent preset (ADR-0053). Identity was
    // supplied to the constructor above (immutable past build); this applies
    // the profile's capability scope, operation boundary, runtime knobs, and
    // attended flag in one call. The profile makes the role declarative.
    agent.apply_preset(&preset);

    // Wire the `[agent]` config table (or legacy `[master]`): the opt-in hard-stop
    // budget, the model-supplied-stdin toggle, the interactive-input-panel opt-out,
    // and the anti-anchoring nudge config. All default to sensible values
    // when the table is absent, so this is a no-op for the common case.
    // These run *after* the profile binding so per-installation config wins.
    // ADR-0141: bind the human-channel posture source. With an accountant
    // the agent reads the OR of attached clients (live). Without one
    // (one-shot CLI paths that never attach) the static posture from the
    // startup flags applies — headless no-TTY bootstraps Autonomous.
    if let Some(accountant) = human_channel.clone() {
        agent.set_human_channel_accountant(accountant);
    } else {
        // One-shot paths: a TUI bootstrap is interactive by construction;
        // headless (`-p` runs, remote automation) declares Autonomous.
        agent.set_human_posture(muta_contracts::human_request::HumanChannelPosture::Interactive);
    }
    agent.set_hard_stop_turns(config.agent.hard_stop_turns);
    agent.set_doom_guard_config(config.agent.doom_guard);
    agent.set_allow_model_stdin(config.agent.allow_model_stdin);
    agent.set_skip_interactive_input(config.agent.skip_interactive_input);
    agent.set_autonomous_fallback_policy(config.agent.ask_user_fallback);
    // Bash safety is action-based and independent from project-extension trust.
    // Workspace authority is enforced by the permission chain; unconditional
    // destructive denies and explicit high-risk confirmations remain here.
    agent.set_bash_policy(&config.bash_policy);

    // Lifecycle event hooks (ADR-0025): each `[[hooks]]` entry runs a shell
    // command at one lifecycle point (PreToolUse / PostToolUse / Stop / …).
    agent.set_hooks(crate::hooks::build_hook_registry(&config.hooks, &agent));

    // Tie the agent to this session/thread.
    let thread_id = session.id().await;
    agent.set_thread_id(&thread_id);

    // Restore the session-scoped runtime state and fire SessionStart hooks.
    // Skipped entirely in Picker mode: the bootstrap session is a throwaway
    // fresh one, and the user has not chosen a session yet. The full restore
    // (todos + disabled tools + round counter + the delegated flag + SessionStart
    // hooks) runs when a real session is opened from the picker — see
    // `handlers_slash`'s `restore_session_runtime`.
    if !is_picker {
        // Restore the unified task list so resume re-shows the sticky panel with
        // the same items (and identity) the model last persisted. An empty list
        // is the "no active task list" state and needs no restore.
        let persisted_todos = session.todos().await;
        if !persisted_todos.is_empty() {
            agent.set_todos(persisted_todos);
        }

        // Restore the remaining session-scoped runtime state (ADR-0048 Phase 2):
        // the orthogonal tool mask and round counter.
        agent.restore_disabled_tools(session.disabled_tools().await);
        agent.restore_round_count(session.round_counter().await);

        // Restore the session-scoped unattended posture (ADR-0132). This is
        // the daemon-restart recovery path: a session that died unattended
        // reopens unattended — attach, lazy-resume, and boot rehost all flow
        // through here. `--unattended` ran earlier and may already have set
        // the flag live; the store read is idempotent either way (same
        // value, and `set_unattended` on the store is a no-op guard), but the
        // explicit flag above wins when both apply, matching the user's most
        // recent explicit intent.
        let persisted_unattended = session.unattended().await;
        if persisted_unattended && !agent.unattended() {
            agent.set_unattended(true);
            let restored_session_id = session.id().await;
            tracing::info!(
                session = %restored_session_id,
                "restored unattended-mode posture from session store"
            );
        }

        // SessionStart hooks (ADR-0025): inject setup context before the first
        // round. Resume vs fresh start is surfaced so a hook can branch.
        {
            let source = match &startup {
                SessionStart::Resume(_) => muta_contracts::SessionSource::Resume,
                _ => muta_contracts::SessionSource::Startup,
            };
            let mut messages = session.model_window().await;
            agent.fire_session_start(source, &mut messages).await;
            // Persist the hook-injected setup context through the single write
            // path so the session stays the source of truth (ADR-0048).
            if let Err(err) = session.replace_messages(messages).await {
                tracing::warn!(error = %err, "failed to persist SessionStart hook context");
            }
        }
    }

    // Load per-model usage telemetry (recency signal for the picker,
    // ADR-0002 phase 2). Moved into the agent task so both the startup
    // activation and runtime switches record through one instance.
    let provider_usage = shared_provider_usage.read().await.clone();

    // Primary round lifecycle: at most one active round, superseded by the
    // next begin (replaces the old token-slot + generation-counter pair).
    let lifecycle = Arc::new(RoundLifecycle::new());
    let req_tx_for_commands = req_tx.clone();
    // `/btw` aside state (ADR-0017, lifted to a multi-slot registry by
    // ADR-0103). The primary round machinery is left exactly as-is; the
    // registry peers it with any number of live asides + an explicit
    // "which aside is the composer targeting" pointer that routes `Chat` to
    // whichever session the user is currently composing into. Leaving an
    // aside view detaches non-destructively — the aside keeps running.
    let side: Arc<AsyncRwLock<crate::side::SideRegistry>> =
        Arc::new(AsyncRwLock::new(crate::side::SideRegistry::new()));
    let base_tools_for_side = base_tools.clone();
    let project_root_for_side = project_root.clone();

    // Initial values for the frontend
    let initial_provider_name = catalog::default_provider_id(&config).to_string();
    let initial_model_name =
        catalog::resolved_model_name_with_usage(&config, &initial_provider_name, &provider_usage)
            .unwrap_or_default();

    // Keep an Arc handle for the caller so SessionEnd hooks (ADR-0025) can
    // fire after its UI returns — the driver below moves `agent`.
    let agent_for_session_end = Arc::clone(&agent);
    // Shared token-source ledger: the agent books each turn's token usage
    // (reported vs. estimated) into it, and the frontend reads it for the
    // token-source report.
    let token_ledger = muta_contracts::TokenSourceLedger::shared();
    // Durable cross-session usage mirror (ADR-0122): every terminal settle is
    // forwarded into the day-partitioned store under `data/usage/` — a
    // sibling of `projects/`, so session cleanup can never touch it.
    token_ledger.install_usage_sink(Arc::new(
        muta_persistence::usage_stats::UsageStatsStore::new(),
    ));
    token_ledger.set_usage_project(muta_persistence::paths::project_bucket_name(&project_root));
    subagent_tool_handle.bind_accounting(
        token_ledger.clone(),
        agent.thread_id_handle(),
        agent.round_counter_handle(),
    );

    // Forward background job lifecycle events directly into the frontend response stream
    {
        let mut job_rx = background_jobs.subscribe();
        let resp_tx_jobs = resp_tx.clone();
        let session_for_jobs = session.clone();
        tokio::spawn(async move {
            let session_id = session_for_jobs.id().await;
            while let Ok(event) = job_rx.recv().await {
                let round_evt = match event {
                    crate::background_jobs::BackgroundJobEvent::Started(info) => {
                        RoundEvent::BackgroundJobStarted(info)
                    }
                    crate::background_jobs::BackgroundJobEvent::Progress { job_id, line } => {
                        RoundEvent::BackgroundJobProgress { job_id, line }
                    }
                    crate::background_jobs::BackgroundJobEvent::Ready { job_id } => {
                        RoundEvent::BackgroundJobReady { job_id }
                    }
                    crate::background_jobs::BackgroundJobEvent::Completed(outcome) => {
                        RoundEvent::BackgroundJobCompleted(outcome)
                    }
                };
                let _ = resp_tx_jobs.send(round_response(&session_id, round_evt));
            }
        });
    }

    // ADR-0190 mailbox: fabric completion events wake the session. The
    // driver's round machinery decides queue-vs-start via `RoundLifecycle`;
    // this task is the select-arm body running outside the request loop so
    // wake requests contend with user input on equal terms.
    {
        let mailbox_rx = background_jobs.subscribe();
        let mailbox_env = crate::task_mailbox::MailboxEnv {
            side: side.clone(),
            agent: agent.clone(),
            session: session.clone(),
            lifecycle: lifecycle.clone(),
            tx: resp_tx.clone(),
            req_tx: req_tx.clone(),
            config: config.clone(),
        };
        let mailbox_id = session.id().await;
        tokio::spawn(async move {
            crate::task_mailbox::run_mailbox(mailbox_rx, mailbox_env, mailbox_id).await;
        });
    }

    let driver = SessionDriver {
        req_rx,
        tx: resp_tx,
        req_tx: req_tx_for_commands,
        agent,
        session: session.clone(),
        config: shared_config,
        provider_usage: shared_provider_usage,
        provider_holder: provider_for_task,
        skills_registry,
        subagent_registry,
        mcp_runtime,
        workspace_security: workspace_security.clone(),
        shared_additional_roots: shared_additional_roots.clone(),
        shared_confinement: shared_confinement.clone(),
        command_catalog: command_catalog.clone(),
        lifecycle,
        side,
        base_tools: base_tools_for_side,
        project_root: project_root_for_side,
        startup,
        open_picker_on_start,
        ui,
        token_ledger: token_ledger.clone(),
        extra_commands: Arc::new(crate::slash_handler::SlashCommandRegistry::new()),
        websearch_shared,
        background_jobs,
    };

    Ok(Bootstrap {
        driver,
        req_tx,
        resp_rx,
        agent_for_session_end: agent_for_session_end.clone(),
        session,
        token_ledger,
        initial_provider_name,
        initial_model_name,
        restored_messages,
        command_catalog,
        agent: agent_for_session_end.clone(),
        security: workspace_security.clone(),
        shared_additional_roots,
        shared_confinement,
    })
}
