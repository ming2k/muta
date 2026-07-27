//! The shared session-harness factory for every frontend binary (ADR-0037
//! Step 6).
//!
//! [`assemble`] performs the full session startup that used to live inline in
//! the `neenee` binary's `main`: channel creation, custom-command discovery,
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
//! `StartupMode::Doctor`, `StartupMode::Attach`, and `StartupMode::Showcase`
//! are **not** handled here: they are purely local (or client-side)
//! short-circuits and must be dispatched by the caller before invoking
//! [`assemble`].

use crate::commands::{CustomCommand, discover_commands};
use neenee_agent::catalog;
use neenee_agent::orchestration::{
    MidTurnPruneProjectionGate, ProxyProvider, round_response, start_repeat_scheduler,
};
use neenee_agent::{Agent, AgentIdentity, EnvoyTool, PrincipalProfile, RoundLifecycle};
use neenee_core::{
    AgentRequest, AgentResponse, CHARS_PER_TOKEN, EXPLORE, Message, Provider, RoundEvent,
    ToolContextBuilder, ToolSet, collect_toolset,
};
use neenee_agent::mcp::{McpCatalog, McpRuntime};
use neenee_persistence::{
    RepeatStore,
    config::{Config, TuiConfig},
    embedding, lock, paths, provider_usage,
    session::SessionStore,
};
use neenee_skills::{SkillCatalog, SkillRegistry};

use crate::startup::{BuiltinCmd, StartupMode};
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
    /// How the session should start. Only `Fresh`, `Resume`, and `Picker`
    /// reach the harness; `Doctor`, `Attach` (and debug-only `Showcase`) must
    /// be short-circuited by the caller before calling [`assemble`].
    pub startup: StartupMode,
    /// `--project` override; when `None`, the current directory is used.
    pub project_root: Option<PathBuf>,
    /// `--unattended` at start: the agent runs without human intervention.
    pub unattended: bool,
    /// `--single-instance`: restore the pre-ADR-0018 exclusive per-project
    /// process lock.
    pub single_instance: bool,
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
    pub token_ledger: Arc<neenee_core::TokenSourceLedger>,
    /// The provider name the UI should display at startup.
    pub initial_provider_name: String,
    /// The model name the UI should display at startup.
    pub initial_model_name: String,
    /// Persisted input history for the frontend's composer.
    pub input_history: Vec<String>,
    /// The session's restored transcript (empty for a fresh session).
    pub restored_messages: Vec<Message>,
    /// `(slash-name, description)` pairs for user-defined `/<name>` commands,
    /// for the frontend's completion/help.
    pub custom_command_suggestions: Vec<(String, String)>,
    /// The `[tui]` presentation config, pulled out of the live config before
    /// the driver takes ownership of it.
    pub tui_config: TuiConfig,
    /// The per-project advisory process lock (ADR-0018), when
    /// `--single-instance` requested it. **The guard releases on drop** — the
    /// caller must hold it for the process lifetime (e.g. bind it to
    /// `let _process_lock = ...` in `main`).
    pub process_lock: Option<lock::ProcessLock>,
}

/// Assemble one live session harness. See the module docs for the contract.
///
/// The ordering and background-spawn behavior are identical to the original
/// inline `main`: live model discovery, skill catalog
/// refresh, MCP connect/refresh, and the repeat scheduler (which holds a
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
        unattended: unattended_at_start,
        single_instance,
    } = params;
    debug_assert!(
        matches!(
            startup,
            StartupMode::Fresh | StartupMode::Resume(_) | StartupMode::Picker
        ),
        "assemble only handles Fresh/Resume/Picker; Doctor, Attach, and Showcase must short-circuit in the caller"
    );

    // First-run friendliness: this harness opens some stores eagerly (the
    // repeat scheduler's SQLite db under data_dir) and does not create their
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
    let custom_commands = discover_commands()
        .into_iter()
        .filter(|command| {
            !BuiltinCmd::ALL
                .iter()
                .any(|(name, _)| *name == command.name)
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
                        .description
                        .clone()
                        .unwrap_or_else(|| "Run project command".to_string()),
                )
            })
            .collect::<Vec<_>>();
        suggestions.sort_by(|left, right| left.0.cmp(&right.0));
        suggestions
    };

    let mut config = Config::load();
    if catalog::migrate_legacy_provider_instances(&mut config)
        && let Err(error) = config.save()
    {
        tracing::warn!(?error, "could not persist provider instance migration");
    }
    // Reconcile template-sourced instances before building the picker: Fixed
    // instances mirror their template, while Api instances retain their last
    // discovered client-supported subset. Pure-custom instances (no
    // `template_id`) are untouched. See the `reconcile_provider_models` doc
    // comment for the exact semantics.
    if catalog::reconcile_provider_models(&mut config)
        && let Err(error) = config.save()
    {
        tracing::warn!(?error, "could not persist provider model reconciliation");
    }
    // Overlay persisted fitted-model metadata onto model resolution, so ids a
    // trusted provider advertised (but the static registry does not know)
    // resolve with their real capabilities from the very first request.
    catalog::sync_fitted_model_registry(&config);

    // Live model-list discovery for API-sourced instances. Runs in the
    // BACKGROUND so slow/unreachable providers never delay the first frame:
    // every instance already has either its fixed snapshot or last known valid
    // subset. The live `GET /models` result is intersected with the client's
    // protocol-compatible model registry (or, for fitting-enabled trusted
    // templates, materialized wholesale with capability metadata); failure or
    // an empty intersection leaves that subset untouched. The task persists
    // only actual changes, so the refreshed list takes effect on the next
    // session. See
    // `discover_provider_models` for the exact rules.
    tokio::spawn(async move {
        let mut config = Config::load();
        let outcome = catalog::discover_provider_models(&mut config).await;
        if outcome.changed {
            catalog::sync_fitted_model_registry(&config);
            if let Err(error) = config.save() {
                tracing::warn!(?error, "live discovery: could not persist refreshed models");
            }
        }
        // Startup discovery failures are background-only; log them so the cause
        // is observable without a UI channel here.
        for (provider, message) in &outcome.failures {
            tracing::warn!(provider = %provider, error = %message, "startup live model discovery failed");
        }
    });

    // Resolve the project root early: it feeds the per-project lock, the
    // session store, and the embedding index. CLI parsing happened in the
    // caller (showcase/doctor already returned there).
    let project_root = project_override.clone().unwrap_or_else(|| {
        std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
    });

    // Durable store for `/repeat` cron jobs. Opened once; cloned for the
    // command handler and the background scheduler.
    //
    // Open it concurrently with the independent `EmbeddingStore::open` (a file
    // read for the semantic-search index), so the two blocking I/O opens run
    // in parallel instead of sequentially.
    let (repeat_store, embedding_store) = tokio::try_join!(
        RepeatStore::open(paths::get().repeat_db()),
        embedding::EmbeddingStore::open(
            paths::get().project_embeddings(&project_root),
            Arc::new(embedding::MockEmbeddingProvider::new(384)),
        ),
    )?;
    let embedding_store: Arc<AsyncRwLock<embedding::EmbeddingStore>> =
        Arc::new(AsyncRwLock::new(embedding_store));
    // Background scheduler: every 30s prune expired jobs and fire any that are
    // due, dispatching each prompt as a normal chat round.
    start_repeat_scheduler(
        repeat_store.clone(),
        req_tx.clone(),
        std::time::Duration::from_secs(30),
    );

    // Initialize Agent logic. The provider is resolved through the model
    // catalog (`build_provider_for`), the single source of truth for the
    // env-var-then-config resolution rules shared with runtime switching. See
    // `docs/adr/0002-model-channel-abstraction.md`.

    // ADR-0018: the per-project advisory lock is opt-in. The default is
    // unlocked so multiple instances can run in one project — each pins its
    // own `sessions/<id>.{json,jsonl}` and never shares a mutable file.
    // `--single-instance` restores the pre-0018 exclusive lock for users who
    // want it. Doctor always skips the lock so it can inspect stores while
    // another instance is running (the caller short-circuits Doctor before
    // calling `assemble`, so the guard below is belt-and-braces).
    let process_lock = if single_instance && !matches!(startup, StartupMode::Doctor) {
        Some(lock::ProcessLock::acquire(
            &paths::get().project_lock_file(&project_root),
        )?)
    } else {
        None
    };

    // Showcase: render a single UI component standalone. No agent, session, or
    // network — just the component's model + renderer on a live terminal.
    // The caller returns for it before calling `assemble`, before any of the
    // agent/session plumbing here is constructed.
    #[cfg(debug_assertions)]
    debug_assert!(
        !matches!(startup, StartupMode::Showcase(_)),
        "showcase must return before the agent/session plumbing runs"
    );

    // Session loading honors the startup mode. Under ADR-0018
    // `load_for_project` pins a fresh `sessions/<id>.{json,jsonl}`, so a bare
    // start always begins a new session; prior sessions stay on disk and are
    // reachable through the picker or `resume`. Resume opens an existing
    // session file by id.
    let session = Arc::new(SessionStore::load_for_project(project_root.clone()));
    let open_picker_on_start = match &startup {
        StartupMode::Fresh => false,
        StartupMode::Picker => true,
        StartupMode::Resume(id) => {
            if let Err(error) = session.resume(id.as_deref()).await {
                eprintln!("resume failed: {error}; starting a fresh session.");
            }
            false
        }
        StartupMode::Doctor => unreachable!("doctor returns before this match"),
        StartupMode::Attach(_) => unreachable!("attach returns before this match"),
        #[cfg(debug_assertions)]
        StartupMode::Showcase(_) => unreachable!("showcase returns before this match"),
    };

    // C6: overlay the session's provider/model pin onto the effective config
    // before building the initial provider. A session that previously ran
    // `/models` reopens on its own provider instead of the global default,
    // so one session's choice never bleeds into another. Done after the session
    // is loaded (and, for resume, after `resume` swapped in its data).
    if let Some(selection) = session.provider_selection().await {
        config.default_provider = selection.provider;
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
            .unwrap_or_else(|| Arc::new(neenee_agent::NoProvider));

    let provider_holder = Arc::new(RwLock::new(initial_provider));
    let provider_for_task = provider_holder.clone();

    let agent_provider = Arc::new(ProxyProvider::new(provider_holder));

    // Shared skills registry for the skill tools. The registry starts EMPTY so
    // discovering skills (scanning local dirs, cloning/fetching remote repos)
    // never blocks the first frame; the background refresh loop re-scans all
    // sources immediately on spawn and then every hour. The `Arc` is shared
    // across the skill tools, the envoy profile, and the frontend, so once the
    // background load lands they all observe the populated state.
    let skills_registry = Arc::new(SkillRegistry::empty_with_config(&config.skills));
    neenee_agent::dynamic::spawn_refresh(SkillCatalog::new((*skills_registry).clone()));

    // Built-in tools self-register via `inventory` (most tools carry a
    // `register_tool!` submission at its definition site) and are collected
    // here from a single opaque context. Tools that need runtime state (the web
    // tools' search config, the shared skill registry, the embedding index +
    // session store) pull it out of the context by type — see
    // `neenee_core::tool_registry`. Stateful/meta tools that genuinely depend on the
    // *rest* of the toolset (the envoy dispatch `task`) cannot
    // self-register and are assembled explicitly below. MCP tools are
    // discovered at runtime and published directly to the principal Agent;
    // they are not part of this static capability set.
    let tool_ctx = {
        let mut builder = ToolContextBuilder::new();
        builder.provide(config.websearch.clone());
        builder.provide(skills_registry.clone());
        builder.provide(embedding_store.clone());
        builder.provide(session.clone());
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
    let base_tools: Arc<Vec<Arc<dyn neenee_core::Tool>>> = Arc::new(toolset.default_view());
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
    // Wire the per-project "always allow" allowlist so prior `Always`
    // approvals survive across sessions in this project. Best-effort: a
    // missing or unreadable permissions.json just means we re-prompt.
    agent.set_project_root(Some(project_root.clone()));
    // Seed declarative permission rules from `[permissions]` config so default
    // policies are data-driven. Runtime "Always" decisions still write to
    // permissions.json; these config rules re-apply on every start.
    agent.seed_permissions_from_config(&config.permissions.allow);
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
    neenee_agent::dynamic::spawn_refresh(McpCatalog::new(mcp_runtime.clone()));
    if unattended_at_start {
        agent.set_unattended(true);
        let _ = resp_tx.send(round_response(
            &session.id().await,
            RoundEvent::Text(
                "Unattended ON: the agent will run without human intervention (no confirmations, no questions).".to_string(),
            ),
        ));
    }

    // Kick off the two independent file reads on the blocking pool NOW so they
    // run concurrently with the agent seeding, pursuit restore, and todo
    // restore below (rather than serially blocking the executor). Both read
    // from `paths::get()` globals and return owned, `Send` data, so a plain
    // `spawn_blocking` closure is self-contained. They are awaited later where
    // their results feed the harness / frontend.
    let input_history_handle = tokio::task::spawn_blocking(Config::load_history);
    let provider_usage_handle = tokio::task::spawn_blocking(provider_usage::ProviderUsage::load);

    let restored_messages = session.full_transcript().await;

    // Mid-turn context projection: when pruning is enabled, install a gate that
    // clears old tool results between ReAct turns once pressure crosses the
    // prune threshold. The threshold is derived from the active model's context
    // window and re-seeded whenever the provider switches (see
    // `reseed_prune_threshold`), so it tracks the live model rather than a
    // fixed character budget.
    if config.compaction_prune {
        agent.set_context_projection_gate(Some(Arc::new(MidTurnPruneProjectionGate {
            session: session.clone(),
            prune_protect_chars: config.compaction_prune_protect_tokens * CHARS_PER_TOKEN,
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
    // model-supplied-stdin toggle, and the anti-anchoring nudge config. (Session
    // review is on-demand via `/review`, so it has no config to seed.) All
    // default to sensible values when the table is absent, so this is a no-op
    // for the common case — the nudge config defaults to disabled. These run
    // *after* the profile binding so per-installation config wins.
    agent.set_hard_stop_turns(config.principal.hard_stop_turns);
    agent.set_doom_guard_config(config.principal.nudge);
    agent.set_allow_model_stdin(config.principal.allow_model_stdin);
    agent.set_bash_policy(&config.bash_policy);

    // Lifecycle event hooks (ADR-0025): each `[[hooks]]` entry runs a shell
    // command at one lifecycle point (PreToolUse / PostToolUse / Stop / …).
    agent.set_hooks(crate::hooks::build_hook_registry(&config.hooks));

    // Tie the agent to this session/thread.
    let thread_id = session.id().await;
    agent.set_thread_id(&thread_id);

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

    // SessionStart hooks (ADR-0025): inject setup context before the first
    // round. Resume vs fresh start is surfaced so a hook can branch.
    {
        let source = match &startup {
            StartupMode::Resume(_) => neenee_core::SessionSource::Resume,
            _ => neenee_core::SessionSource::Startup,
        };
        let mut messages = session.model_window().await;
        agent.fire_session_start(source, &mut messages).await;
        // Persist the hook-injected setup context through the single write
        // path so the session stays the source of truth (ADR-0048).
        if let Err(err) = session.replace_messages(messages).await {
            tracing::warn!(error = %err, "failed to persist SessionStart hook context");
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
    let repeat_store_for_commands = repeat_store.clone();
    let req_tx_for_commands = req_tx.clone();
    // `/btw` side-conversation state (ADR-0017). The primary round machinery is
    // left exactly as-is; this slot peers it with an optional live side
    // session + an "active view" flag that routes `Chat` to whichever session
    // the user is currently composing into.
    let side: Arc<AsyncRwLock<Option<crate::side::SideSession>>> = Arc::new(AsyncRwLock::new(None));
    let active_view_side = Arc::new(std::sync::atomic::AtomicBool::new(false));
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
    // Keep an Arc handle for the caller so SessionEnd hooks (ADR-0025) can
    // fire after its UI returns — the driver below moves `agent`.
    let agent_for_session_end = Arc::clone(&agent);
    // Shared token-source ledger: the agent books each turn's token usage
    // (reported vs. estimated) into it, and the frontend reads it for the
    // token-source report.
    let token_ledger = neenee_core::TokenSourceLedger::shared();
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
        commands: commands_for_task,
        embedding_store: embedding_store_for_commands,
        repeat_store: repeat_store_for_commands,
        lifecycle,
        side,
        active_view_side,
        base_tools: base_tools_for_side,
        project_root: project_root_for_side,
        startup,
        open_picker_on_start,
        ui,
        token_ledger: token_ledger.clone(),
        extra_commands: Arc::new(crate::slash_handler::SlashCommandRegistry::new()),
    };

    Ok(Bootstrap {
        driver,
        req_tx,
        resp_rx,
        agent_for_session_end,
        session,
        token_ledger,
        initial_provider_name,
        initial_model_name,
        input_history,
        restored_messages,
        custom_command_suggestions,
        tui_config,
        process_lock,
    })
}
