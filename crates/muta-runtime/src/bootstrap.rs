//! The shared session-harness factory for every frontend binary (ADR-0037
//! Step 6).
//!
//! [`assemble`] performs the full session startup that used to live inline in
//! the `muta` binary's `main`: channel creation, custom-command discovery,
//! config load + migrations, background live model discovery, store opens,
//! the repeat scheduler, provider/skills/toolset wiring, `EnvoyTool` layering,
//! agent construction, MCP background connect, pursuit/todo/session-state
//! restore, and finally [`SessionDriver`] construction — in the exact order
//! the original `main` did, with the same background spawns.
//!
//! The crate stays application-neutral (ADR-0054): the caller supplies the
//! [`AgentIdentity`], the [`PrincipalProfile`], and the [`UiBridge`] as
//! parameters. Nothing here names a product or a principal.
//!
//! `SessionStart::Version`, `SessionStart::Doctor`, `SessionStart::Attach`, and
//! `SessionStart::Showcase` are **not** handled here: they are purely local
//! (or client-side) short-circuits and must be dispatched by the caller
//! before invoking [`assemble`].

use crate::commands::{CustomCommand, discover_commands_with_extensions};
use muta_agent::catalog;
use muta_agent::orchestration::{MidTurnPruneProjectionGate, ProxyProvider, round_response};
use muta_agent::{Agent, AgentIdentity, EnvoyTool, PrincipalProfile, RoundLifecycle};
use muta_contracts::{
    AgentNotice, AgentRequest, AgentResponse, EXPLORE, Message, NoticeKind, NoticeSeverity,
    NoticeSource, NoticeSurface, Provider, RoundEvent, ToolContextBuilder, ToolSet,
    WorkspaceExecutionProfile, WorkspaceExtensionsState, WorkspaceSandboxState, collect_toolset,
};
use muta_mcp::{McpCatalog, McpRuntime};
use muta_persistence::{
    config::{Config, InputHistoryConfig, TuiConfig},
    connection_usage, embedding, paths,
    session::SessionStore,
    workspace_security::WorkspaceSecurityStore,
};
use muta_skills::{SkillCatalog, SkillRegistry};

use crate::startup::{BuiltinCmd, SessionStart};
use crate::{SessionDriver, UiBridge};

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::RwLock;
use tokio::sync::{RwLock as AsyncRwLock, mpsc};

/// Everything a frontend binary must supply to assemble a session harness.
///
/// The identity and principal are the *only* application-specific inputs; all
/// other behavior is shared across frontends.
pub struct BootstrapParams {
    /// The agent's identity (name + mission), bound at construction.
    pub identity: AgentIdentity,
    /// The declarative principal profile (ADR-0053), applied after
    /// construction and before the `[principal]` config overlay.
    pub principal: PrincipalProfile,
    /// The frontend's clipboard/UI bridge (used by `/export`).
    pub ui: Arc<dyn UiBridge>,
    /// How the session begins (ADR-0116: only the assembly-relevant
    /// shapes exist here; one-shot CLI modes never reach the harness).
    pub startup: SessionStart,
    /// `--project` override; when `None`, the current directory is used.
    pub project_root: Option<PathBuf>,
    /// `--autopilot` at start: the agent runs without human intervention.
    pub autopilot: bool,
    /// Extra tools to publish onto the assembled agent (ADR-0097 §5's
    /// WIP-coordination tools). The assemble passes them through untouched;
    /// the registry publishes them once the session id is known.
    pub extra_session_tools: Option<Vec<Arc<dyn muta_contracts::Tool>>>,
    /// Session-lifetime cancellation token (ADR-0125): passed through to the
    /// background `/schedule` scheduler so it stops when the harness is torn
    /// down (suspension, kill, daemon drain) instead of ticking forever.
    /// `None` = process-lifetime scheduling (single-session frontends).
    pub teardown_token: Option<tokio_util::sync::CancellationToken>,
}

/// The assembled session harness: the driver (ready to `run`), the frontend
/// ends of the request/response channels, and the values the frontend needs
/// to start its UI and wind the session down.
pub struct Bootstrap {
    /// The session driver, fully wired. The caller moves it into a task
    /// (`tokio::spawn(driver.run())`).
    pub driver: SessionDriver,
    /// The frontend's request sender (the driver holds the receiver).
    pub req_tx: mpsc::UnboundedSender<AgentRequest>,
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
    /// Persisted input history for the frontend's composer. Each entry is
    /// tagged with the session/workspace it came from so Ctrl+R can search
    /// the whole history while inline ↑/↓ walks only the current session's.
    pub input_history: Vec<muta_contracts::HistoryEntry>,
    /// The session's restored transcript (empty for a fresh session).
    pub restored_messages: Vec<Message>,
    /// Complete daemon-owned command/completion vocabulary for this session.
    pub command_catalog: muta_contracts::CommandCatalog,
    /// The `[tui]` presentation config, pulled out of the live config before
    /// the driver takes ownership of it.
    pub tui_config: TuiConfig,
    /// The `[input_history]` config, pulled out of the live config alongside
    /// `tui_config` so the frontend can dedup / filter history as it records.
    pub input_history_config: InputHistoryConfig,
    /// caller must hold it for the process lifetime (e.g. bind it to
    /// Echo of [`BootstrapParams::extra_session_tools`], for the registry to
    /// publish once the session id is known. Not consumed by the assemble.
    pub extra_session_tools: Option<Vec<Arc<dyn muta_contracts::Tool>>>,
    /// The primary agent (same `Arc` as `agent_for_session_end`), exposed so
    /// the registry can publish session-scoped tools onto it.
    pub agent: Arc<Agent>,
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
        principal,
        ui,
        startup,
        project_root: project_override,
        autopilot: autopilot_at_start,
        extra_session_tools,
        teardown_token,
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

    let (req_tx, req_rx) = mpsc::unbounded_channel::<AgentRequest>();
    let (resp_tx, resp_rx) = mpsc::unbounded_channel::<AgentResponse>();

    let mut config = Config::load();
    // One-shot migration from the legacy layout (provider instances in
    // `config.toml`, keys in `[builtins]/[user]`) to the state stores. Runs
    // before anything touches the instance store.
    catalog::migrate_legacy_state();
    // Overlay persisted fitted-model metadata onto model resolution, so ids a
    // trusted provider advertised (but the static registry does not know)
    // resolve with their real capabilities from the very first request.
    catalog::sync_fitted_model_registry();

    // Live model-list discovery for API-sourced instances. Runs in the
    // BACKGROUND so slow/unreachable providers never delay the first frame:
    // every instance already has either its fixed snapshot or last known valid
    // subset. The live `GET /models` result is intersected with the client's
    // protocol-compatible model registry (or, for fitting-enabled trusted
    // templates, materialized wholesale with capability metadata); failure or
    // an empty intersection leaves that subset untouched. The session driver
    // handles the refresh and broadcasts updated snapshots to the client.
    let req_tx_for_discovery = req_tx.clone();
    tokio::spawn(async move {
        let _ = req_tx_for_discovery.send(AgentRequest::RefreshProviderModels {
            user_initiated: false,
        });
    });

    // Resolve the project root early: it feeds the per-project lock, the
    // session store, and the embedding index. CLI parsing happened in the
    // caller (showcase/doctor already returned there).
    let project_root = project_override.clone().unwrap_or_else(|| {
        std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
    });

    // Open the per-project embedding index (a file read for the
    // semantic-search index) concurrently with nothing else heavy; it feeds
    // `/search` and the agent's retrieval tools.
    let embedding_store = embedding::EmbeddingStore::open(
        paths::get().project_embeddings(&project_root),
        Arc::new(embedding::MockEmbeddingProvider::new(384)),
    )
    .await?;
    let embedding_store: Arc<AsyncRwLock<embedding::EmbeddingStore>> =
        Arc::new(AsyncRwLock::new(embedding_store));

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
    muta_agent::orchestration::start_supervised_schedule_scheduler(
        Arc::clone(&session),
        req_tx.clone(),
        std::time::Duration::from_secs(30),
        teardown_token.clone(),
    );

    // C6: overlay the session's provider/model pin onto the effective config
    // before building the initial provider. A session that previously ran
    // `/models` reopens on its own provider instead of the global default,
    // so one session's choice never bleeds into another. Done after the session
    // is loaded (and, for resume, after `resume` swapped in its data).
    if let Some(selection) = session.provider_selection().await {
        config.default_connection = selection.provider;
        if let Some(model) = selection.model {
            config.default_model = Some(model);
        }
    }

    // The catalog returns `None` when no real channel resolves (empty config or
    // an unknown default). Install the explicit `NoProvider` sentinel so the
    // holder type is satisfied; the chat dispatch refuses up-front with a
    // user-facing notification while this sentinel is live.
    let initial_provider: Arc<dyn Provider> =
        catalog::build_provider_for(&config, catalog::default_provider_id(&config))
            .unwrap_or_else(|| Arc::new(muta_agent::NoProvider));

    let provider_holder = Arc::new(RwLock::new(initial_provider));
    let provider_for_task = provider_holder.clone();

    let agent_provider = Arc::new(ProxyProvider::new(provider_holder));

    // Shared skills registry for the skill tools. The registry starts EMPTY so
    // discovering skills (scanning local dirs, cloning/fetching remote repos)
    // never blocks the first frame; the background refresh loop re-scans all
    // sources immediately on spawn and then every hour. The `Arc` is shared
    // across the skill tools, the envoy profile, and the frontend, so once the
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
    // `/extensions trust` all report through the same path.
    {
        let resp_tx_for_shadows = resp_tx.clone();
        let session_id_for_shadows = session.id().await;
        skills_registry.set_shadow_sink(Some(Arc::new(move |shadowed| {
            for shadow in shadowed {
                let _ = resp_tx_for_shadows.send(round_response(
                    &session_id_for_shadows,
                    RoundEvent::Notice(
                        AgentNotice::new(
                            NoticeKind::ReviewAlert,
                            NoticeSeverity::Warning,
                            format!(
                                "Project skill '{}' overrides the {}-scope skill of the same name",
                                shadow.name, shadow.overridden_scope
                            ),
                            NoticeSource::Harness,
                        )
                        .with_body(format!(
                            "Loading {} instead. Project-local skills win by priority; \
                             if this is unexpected, inspect the project's skills directories \
                             (.muta/skills, .agents/skills, .claude/skills) or run \
                             `/extensions untrust`.",
                            shadow.winner_source.display()
                        )),
                    ),
                ));
            }
        })));
    }
    muta_agent::dynamic::spawn_refresh(SkillCatalog::new((*skills_registry).clone()));

    // Built-in tools self-register via `inventory` (most tools carry a
    // `register_tool!` submission at its definition site) and are collected
    // here from a single opaque context. Tools that need runtime state (the web
    // tools' search config, the shared skill registry, the embedding index +
    // session store) pull it out of the context by type — see
    // `muta_contracts::tool_registry`. Stateful/meta tools that genuinely depend on the
    // *rest* of the toolset (the envoy dispatch `task`) cannot
    // self-register and are assembled explicitly below. MCP tools are
    // discovered at runtime and published directly to the principal Agent;
    // they are not part of this static capability set.
    // Hot-reloadable `[websearch]` handle: the web tools hold the same `Arc`
    // (via the tool context below), and `UpdateWebSearchConfig` /
    // `/settings reload` write into it, so backend/reader/proxy changes
    // reach the tools on their next call without a toolset rebuild.
    let websearch_shared = Arc::new(muta_contracts::SharedWebSearchConfig::new(
        config.websearch.clone(),
    ));
    let tool_ctx = {
        let mut builder = ToolContextBuilder::new();
        builder.provide(websearch_shared.clone());
        builder.provide(config.websearch.clone());
        builder.provide(skills_registry.clone());
        builder.provide(embedding_store.clone());
        builder.provide(session.clone());
        let execution_env: Arc<dyn muta_contracts::ExecutionEnvironment> = Arc::new(
            muta_agent::execution::WorkspaceExecutionEnvironment::new(project_root.clone()),
        );
        builder.provide(execution_env);
        // The session's workspace root: every workspace-relative tool
        // operation (bash cwd, relative path resolution, search bases)
        // anchors here instead of the daemon process's cwd. Under the
        // unified daemon (ADR-0096) one process hosts sessions for many
        // projects, so the process cwd is whichever directory the first
        // client spawned it from — correct only by coincidence. This is the
        // fix for "launched in project A, session edits project B".
        builder.provide(muta_contracts::WorkspaceRoot(project_root.clone()));
        builder.build()
    };
    let mut toolset: ToolSet = collect_toolset(&tool_ctx);
    // MCP tools are discovered after Agent construction and published through
    // its connector-neutral dynamic-tool sink. The MCP runtime owns protocol
    // and connection state; the agent owns advertisement and dispatch.
    // Snapshot of the shared toolset (built-in default variants) before the
    // `EnvoyTool` is layered on. A `/btw` side session (ADR-0017) rebuilds
    // its `Agent` from this same snapshot — minus its own `EnvoyTool` and
    // without inheriting the principal's session-scoped connector sources.
    let base_tools: Arc<Vec<Arc<dyn muta_contracts::Tool>>> = Arc::new(toolset.default_view());
    // EnvoyTool gets the static capability set (excluding itself) so spawned
    // envoys cannot recurse and inherit the live provider. Dynamic connector
    // sources are principal-only unless a future policy explicitly delegates
    // them. It binds the EXPLORE profile (read-only / non-interactive /
    // non-recursive).
    let envoy_tool = Arc::new(EnvoyTool::new(
        agent_provider.clone(),
        toolset.clone(),
        &EXPLORE,
    ));
    // Envoys resolve relative write-grants against the session's project
    // root, not the daemon process's cwd (ADR-0096).
    envoy_tool.set_workspace_root(Some(project_root.clone()));
    // Envoys inherit the session's connection retry configuration.
    envoy_tool.bind_retry_policy(
        config.connection_retry_max_attempts,
        config.connection_retry_base_ms,
        config.connection_retry_max_ms,
    );
    // Full-duplex (ADR-0029): capture the envoy tool's envoy registry so the
    // request loop can route a user's permission / ask_user reply down into the
    // specific live child that surfaced the request (looked up by the parent
    // tool-call id the frontend tags onto the reply). Captured before
    // `envoy_tool` is layered into the capability set.
    let envoy_registry = envoy_tool.registry();
    // Keep a typed handle so we can bind the parent's variant selection into the
    // envoy tool once the agent (which owns that selection) exists. The same
    // underlying `Arc<EnvoyTool>` is what gets layered into the toolset.
    let envoy_tool_handle = envoy_tool.clone();
    toolset.insert(envoy_tool);
    let agent = Arc::new(
        Agent::builder_from_toolset(agent_provider, toolset, identity)
            .with_skills((*skills_registry).clone())
            .build(),
    );
    // Override axis (model): envoys are agents on the same model, so they
    // inherit the parent's tool-variant selection. The profile still owns the
    // orthogonal scope axis.
    envoy_tool_handle.bind_variant_selection(agent.variant_selection_handle());
    envoy_tool_handle.bind_workspace_security(agent.workspace_security_handle());
    // Wire the per-project "always allow" allowlist so prior `Always`
    // approvals survive across sessions in this project. Best-effort: a
    // missing or unreadable permissions.json just means we re-prompt.
    agent.set_project_root(Some(project_root.clone()));
    // Seed declarative permission rules from `[permissions]` config so default
    // policies are data-driven. Runtime "Always" decisions still write to
    // permissions.json; these config rules re-apply on every start.
    agent.seed_permissions_from_config(&config.permissions.allow);
    // Workspace execution authority and project-extension trust are separate
    // axes. Opening a path grants neither, and extension trust is bound to the
    // current contribution digest rather than the path alone.
    // `.muta/config.toml` may declare `[mcp.*]` servers (which execute
    // processes) and `[[hooks]]` entries (which run shell commands at lifecycle
    // points); its `.muta/skills` and `.muta/commands` trees inject
    // project-authored prompt text (skills can also shadow the user's own
    // same-named skills by priority). Loading those automatically from a
    // cloned or vendored working tree is the same class of hazard as an npm
    // `postinstall` script or a git hook: a malicious repo must not gain code
    // execution — or prompt injection, which for an agent holding tools is
    // execution-by-proxy — merely because the user opened it. The whole
    // package — MCP servers, hooks, project skills AND project slash
    // commands — loads only after the current content has been explicitly
    // trusted. Global config is user-authored and trusted unconditionally.
    let workspace_security = Arc::new(WorkspaceSecurityStore::load());
    let mut security_snapshot = workspace_security.snapshot(&project_root);
    security_snapshot.sandbox = if muta_agent::execution::workspace_sandbox_available() {
        WorkspaceSandboxState::Enforced
    } else {
        WorkspaceSandboxState::Unavailable
    };
    agent.set_workspace_security(security_snapshot.clone());
    let extensions_trusted = security_snapshot.extensions.is_trusted();
    let project_mcp = Config::load_project_mcp(&project_root);
    let project_hooks = Config::load_project_hooks(&project_root);
    let has_project_external = !project_mcp.is_empty() || !project_hooks.is_empty();
    if extensions_trusted {
        if !project_mcp.is_empty() {
            config.merge_project_mcp(project_mcp);
        }
        if !project_hooks.is_empty() {
            config.merge_project_hooks(project_hooks);
        }
    }
    // Skills are gated inside discovery itself (the scan consults the
    // content-bound extension state), so bootstrap only needs the presence
    // checks for the notice; the background refresh finds no Repo-scope
    // sources while they are quarantined.
    if security_snapshot.execution == WorkspaceExecutionProfile::Unknown {
        let _ = resp_tx.send(round_response(
            &session.id().await,
            RoundEvent::Notice(
                AgentNotice::new(
                    NoticeKind::ReviewAlert,
                    NoticeSeverity::Warning,
                    "Workspace trust unconfigured",
                    NoticeSource::Harness,
                )
                .with_surface(NoticeSurface::Banner)
                .with_body(
                    "This workspace has no persisted trust decision. Run `/trust` to authorize \
                     development in this project, or `/trust readonly` for read-oriented analysis.",
                ),
            ),
        ));
    }
    if security_snapshot.execution == WorkspaceExecutionProfile::Development
        && security_snapshot.sandbox == WorkspaceSandboxState::Unavailable
    {
        let _ = resp_tx.send(round_response(
            &session.id().await,
            RoundEvent::Notice(
                AgentNotice::new(
                    NoticeKind::ReviewAlert,
                    NoticeSeverity::Info,
                    "Workspace sandbox is unavailable",
                    NoticeSource::Harness,
                )
                .with_surface(NoticeSurface::Banner)
                .with_body(
                    "Development execution is running with host authority. Shell commands execute \
                     directly in the workspace directory without physical sandbox containment.",
                ),
            ),
        ));
    }
    if matches!(
        security_snapshot.extensions,
        WorkspaceExtensionsState::Quarantined | WorkspaceExtensionsState::Changed
    ) {
        let mut gated: Vec<&str> = Vec::new();
        if has_project_external {
            gated.push("MCP servers and/or hooks in .muta/config.toml");
        }
        if muta_skills::discovery::project_skills_present(&project_root) {
            gated.push("project skills (.muta/skills, .agents/skills, .claude/skills)");
        }
        if crate::commands::project_commands_present(&project_root) {
            gated.push("project slash commands (.muta/commands)");
        }
        if !gated.is_empty() {
            let reason = if security_snapshot.extensions == WorkspaceExtensionsState::Changed {
                "changed since they were trusted"
            } else {
                "have not been trusted"
            };
            let _ = resp_tx.send(round_response(
                &session.id().await,
                RoundEvent::Notice(
                    AgentNotice::new(
                        NoticeKind::ReviewAlert,
                        NoticeSeverity::Warning,
                        "Project extensions are quarantined",
                        NoticeSource::Harness,
                    )
                    .with_surface(NoticeSurface::Banner)
                    .with_body(format!(
                        "This workspace declares {}. Their exact content {reason}; inspect it, \
                         then run `/extensions trust` to load it.",
                        gated.join(", ")
                    )),
                ),
            ));
        }
    }
    // Project-local slash commands (`.muta/commands/`) are prompt-text
    // templates: a malicious repo must not inject `/<name>` commands just
    // because the directory was opened. Only the user-global commands dir
    // loads while project extensions are quarantined; project commands join
    // only when their exact current content is trusted.
    let command_discovery =
        discover_commands_with_extensions(&project_root, security_snapshot.extensions);
    // Shadowing alert: a project command that reuses a user command's name
    // wins by priority — warn once per shadowed name so the override cannot
    // happen silently. Built-in-named entries are skipped: built-ins always
    // win at dispatch, so those project files override nothing.
    for shadow in &command_discovery.shadowed {
        if BuiltinCmd::ALL
            .iter()
            .any(|(name, _)| name.trim_start_matches('/') == shadow.name)
        {
            continue;
        }
        let _ = resp_tx.send(round_response(
            &session.id().await,
            RoundEvent::Notice(
                AgentNotice::new(
                    NoticeKind::ReviewAlert,
                    NoticeSeverity::Warning,
                    format!(
                        "Project command '/{}' overrides the user command of the same name",
                        shadow.name
                    ),
                    NoticeSource::Harness,
                )
                .with_body(format!(
                    "Running /{} uses {}. Project-local commands win by priority; \
                     if this is unexpected, inspect the project's .muta/commands \
                     directory or `/extensions untrust`.",
                    shadow.name,
                    shadow.winner_source.display()
                )),
            ),
        ));
    }
    let custom_commands = command_discovery
        .commands
        .into_iter()
        .filter(|command| {
            // ALL holds slash-prefixed names ("/workspace"); command names are
            // slash-less — compare against the stripped form.
            !BuiltinCmd::ALL
                .iter()
                .any(|(name, _)| name.trim_start_matches('/') == command.name)
        })
        .map(|command| (command.name.clone(), command))
        .collect::<HashMap<String, CustomCommand>>();
    let custom_command_suggestions = {
        let mut suggestions = custom_commands
            .values()
            .map(|command| {
                (
                    format!("/{}", command.name),
                    command
                        .summary
                        .as_ref()
                        .or(command.description.as_ref())
                        .cloned()
                        .unwrap_or_else(|| "Run project command".to_string()),
                )
            })
            .collect::<Vec<_>>();
        suggestions.sort_by(|left, right| left.0.cmp(&right.0));
        suggestions
    };
    let command_catalog = crate::startup::command_catalog(&custom_command_suggestions);
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
    if autopilot_at_start {
        agent.set_autopilot(true);
        // ADR-0132: persist the startup posture onto the session so a later
        // daemon crash/restart restores it. Previously this path never wrote
        // the store (only a live toast), so a `--autopilot` session lost its
        // posture on every restart — and, because the command ledger was
        // never written either, not even the legacy ledger heuristic could
        // recover it. Best-effort: a failed persist degrades to the old
        // process-local behaviour.
        if let Err(error) = session.set_autopilot(true).await {
            tracing::warn!(
                error = %error,
                "could not persist --autopilot startup posture"
            );
        }
        // Surface the `--autopilot` startup state as a transient toast rather
        // than a transcript line: it is a command acknowledgment, not model
        // output, so it should not pollute the conversation the user is about
        // to have. The TUI badge (refreshed by the periodic harness snapshot)
        // keeps the elevated state visible thereafter.
        let _ = resp_tx.send(round_response(
            &session.id().await,
            RoundEvent::Notice(AgentNotice::command_ack(
                "Autopilot ON: no human interaction; approved actions run and missing authority fails immediately.",
            )),
        ));
    }

    // Kick off the two independent file reads on the blocking pool NOW so they
    // run concurrently with the agent seeding, pursuit restore, and todo
    // restore below (rather than serially blocking the executor). Both read
    // from `paths::get()` globals and return owned, `Send` data, so a plain
    // `spawn_blocking` closure is self-contained. They are awaited later where
    // their results feed the harness / frontend.
    let input_history_handle = tokio::task::spawn_blocking(Config::load_history);
    let provider_usage_handle =
        tokio::task::spawn_blocking(connection_usage::ConnectionUsage::load);

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
    if config.compaction_prune {
        agent.set_context_projection_gate(Some(Arc::new(MidTurnPruneProjectionGate {
            session: session.clone(),
            // ADR-0120: token-native — the config key was always tokens; the
            // old ×4 char conversion existed only for the byte-space pruner.
            prune_protect_tokens: config.compaction_prune_protect_tokens,
        })));
        crate::agent_setup::reseed_prune_threshold(&agent, &config);
    }

    // Seed per-model tool-variant selection for the startup model. Each listed
    // capability is realized by its chosen variant in the schemas sent to the
    // provider; re-seeded on provider/model switch.
    crate::agent_setup::reseed_tool_variants(&agent, &config);

    // Bind the caller-supplied principal profile (ADR-0053). Identity was
    // supplied to the constructor above (immutable past build); this applies
    // the profile's capability scope, operation boundary, runtime knobs, and
    // attended flag in one call. The profile makes the role declarative so
    // future principals (quant/research/ops) are another profile, not a fork.
    agent.apply_principal_profile(&principal);

    // Wire the `[principal]` config table: the opt-in hard-stop budget, the
    // model-supplied-stdin toggle, the interactive-input-panel opt-out, and
    // the anti-anchoring nudge config. (Session review is on-demand via
    // `/review`, so it has no config to seed.) All default to sensible values
    // when the table is absent, so this is a no-op for the common case — the
    // nudge config defaults to disabled. These run *after* the profile binding
    // so per-installation config wins.
    agent.set_hard_stop_turns(config.principal.hard_stop_turns);
    agent.set_doom_guard_config(config.principal.nudge);
    agent.set_allow_model_stdin(config.principal.allow_model_stdin);
    agent.set_skip_interactive_input(config.principal.skip_interactive_input);
    // Bash safety is action-based and independent from project-extension trust.
    // Workspace authority is enforced by the permission chain; unconditional
    // destructive denies and explicit high-risk confirmations remain here.
    agent.set_bash_policy(&config.bash_policy);

    // Lifecycle event hooks (ADR-0025): each `[[hooks]]` entry runs a shell
    // command at one lifecycle point (PreToolUse / PostToolUse / Stop / …).
    agent.set_hooks(crate::hooks::build_hook_registry(&config.hooks));

    // Tie the agent to this session/thread.
    let thread_id = session.id().await;
    agent.set_thread_id(&thread_id);

    // Restore the session-scoped runtime state and fire SessionStart hooks.
    // Skipped entirely in Picker mode: the bootstrap session is a throwaway
    // fresh one, and the user has not chosen a session yet. The full restore
    // (todos + disabled tools + round counter + autopilot + SessionStart
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

        // Restore the session-scoped autopilot posture (ADR-0132). This is
        // the daemon-restart recovery path: a session that died unattended
        // reopens unattended — attach, lazy-resume, and boot rehost all flow
        // through here. `--autopilot` ran earlier and may already have set
        // the flag live; the store read is idempotent either way (same
        // value, and `set_autopilot` on the store is a no-op guard), but the
        // explicit flag above wins when both apply, matching the user's most
        // recent explicit intent.
        let persisted_autopilot = session.autopilot().await;
        if persisted_autopilot && !agent.get_autopilot() {
            agent.set_autopilot(true);
            let restored_session_id = session.id().await;
            tracing::info!(
                session = %restored_session_id,
                "restored unattended (autopilot) posture from session store"
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

    // Load history — awaited here after running concurrently with the agent
    // setup above. `unwrap` is safe: `spawn_blocking` only panics if the
    // closure panics, and neither read does.
    let input_history = input_history_handle
        .await
        .unwrap_or_else(|_| Config::load_history());

    // Load per-model usage telemetry (recency signal for the picker,
    // ADR-0002 phase 2). Moved into the agent task so both the startup
    // activation and runtime switches record through one instance.
    let provider_usage = provider_usage_handle.await.unwrap_or_default();

    // Primary round lifecycle: at most one active round, superseded by the
    // next begin (replaces the old token-slot + generation-counter pair).
    let lifecycle = Arc::new(RoundLifecycle::new());
    let commands_for_task = Arc::new(custom_commands);
    let embedding_store_for_commands = embedding_store.clone();
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

    // The driver task takes ownership of `config`; pull the frontend
    // presentation config out first so it can be handed back to the caller.
    let tui_config = config.tui.clone();
    let input_history_config = config.input_history.clone();
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
    envoy_tool_handle.bind_accounting(
        token_ledger.clone(),
        agent.thread_id_handle(),
        agent.round_counter_handle(),
    );

    let driver = SessionDriver {
        req_rx,
        tx: resp_tx,
        req_tx: req_tx_for_commands,
        agent,
        session: session.clone(),
        config,
        provider_usage,
        provider_holder: provider_for_task,
        skills_registry,
        envoy_registry,
        mcp_runtime,
        workspace_security,
        commands: commands_for_task,
        command_catalog: command_catalog.clone(),
        embedding_store: embedding_store_for_commands,
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
        input_history,
        restored_messages,
        command_catalog,
        tui_config,
        input_history_config,
        extra_session_tools,
        agent: agent_for_session_end.clone(),
    })
}
