#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use crate::tui::start_tui;
use neenee_persistence::config::Config;
use neenee_persistence::session;
mod identity;
mod remote;
#[cfg(debug_assertions)]
mod showcase;
mod status;
mod tui;

use neenee_transport::session_view::short_session_id;
pub(crate) use neenee_transport::startup;

/// This CLI's identity, handed to the engine as its opening system prompt.
/// Lives here (not in `neenee-agent`) so the engine stays identity-agnostic
/// and a different frontend could reuse it as another agent.
///
/// The identity constants + [`neenee_identity`] + [`principal_code`] live in
/// this binary's `identity` module (the application layer); the server crate
/// is application-neutral and holds no product name or principal.
use crate::identity::{neenee_identity, principal_code};
use neenee_transport::bootstrap::{self, BootstrapParams};
use neenee_transport::startup::{StartupMode, init_tracing, parse_args};

use std::path::PathBuf;
use std::sync::Arc;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _tracing_guard = init_tracing();
    // Explicit runtime (rather than #[tokio::main]) so exit can use
    // `shutdown_background`: the default drop blocks until every
    // `spawn_blocking` task finishes, which lets a stuck arboard/X11 call
    // (or a contended history `flock`) pin the process for seconds after
    // the user has already quit. `shutdown_background` skips that wait;
    // everything on the exit path that matters (terminal restore, history
    // save) has completed or been bounded before this point.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let result = runtime.block_on(run());
    runtime.shutdown_background();
    result
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    // Parse CLI up front. `showcase` (debug-only) and `doctor` are purely
    // local: no agent, no session, no network. They must short-circuit BEFORE
    // the session harness is assembled — otherwise they would pay the full
    // production startup cost (skill scan, MCP connects,
    // agent construction) for nothing. The Showcase variant only exists under
    // `debug_assertions`, so the guard here mirrors it.
    let (startup, project_override, autopilot_at_start, single_instance) =
        parse_args(std::env::args().skip(1).collect());

    // `--version` is pure metadata: print and exit before any harness, lock,
    // or network work — exactly like the `doctor` short-circuit below.
    if matches!(startup, StartupMode::Version) {
        println!("neenee {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    #[cfg(debug_assertions)]
    if let StartupMode::Showcase(component) = &startup {
        return showcase::run(component);
    }

    // Doctor verifies stored session integrity and exits; it never acquires
    // the per-project lock, so it can run alongside a live instance. Only
    // Fresh/Resume/Picker reach the harness factory below.
    if matches!(startup, StartupMode::Doctor) {
        session::run_doctor(project_override.as_deref()).await?;
        return Ok(());
    }

    // Attach mode drives a session hosted by a `neenee-server` process; the
    // client assembles NO local harness (no config/provider wiring, no stores,
    // no process lock), so it must intercept before `assemble` — which accepts
    // only Fresh/Resume/Picker by contract.
    if let StartupMode::Attach(session_id) = &startup {
        return run_attached(
            session_id.clone(),
            false,
            project_override,
            autopilot_at_start,
            false,
        )
        .await;
    }

    // `neenee serve` runs the headless multi-session host in the foreground
    // (ADR-0094; renamed from the never-released `daemon` of ADR-0089). Like
    // attach, it short-circuits before the local harness.
    if let StartupMode::Serve {
        port,
        public,
        detach,
    } = &startup
    {
        // The unified daemon (ADR-0096) is project-agnostic; run it in the
        // foreground, or fork into the background with --detach.
        if *detach {
            return detach_daemon().map_err(Into::into);
        }
        return neenee_transport::host::run(
            neenee_transport::host::HostIdentity {
                identity: neenee_identity(),
                principal: principal_code(),
                ui: Arc::new(crate::tui::clipboard::TuiClipboard),
            },
            neenee_transport::host::HostOptions {
                port: *port,
                expose: if *public {
                    neenee_transport::serve::ServeExpose::Public
                } else {
                    neenee_transport::serve::ServeExpose::Local
                },
                token: None,
                #[cfg(unix)]
                uds_path: Some(neenee_transport::serve_discovery::default_uds_path()),
            },
        )
        .await;
    }

    // `neenee status` (ADR-0093) observes the project's daemon without
    // spawning one and without assembling any local harness — the control
    // plane stays strictly read-only and client-side.
    if let StartupMode::Status {
        watch,
        json,
        include_idle,
    } = &startup
    {
        let project_root = project_override
            .clone()
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
        return status::run(
            &project_root,
            status::StatusOptions {
                watch: *watch,
                json: *json,
                include_idle: *include_idle,
            },
        )
        .await
        .map_err(Into::into);
    }

    // `neenee dashboard`: the full-screen interactive session dashboard. The
    // client attaches to the daemon's most-recently-active hosted session
    // purely as the underlying TUI carrier, then raises the dashboard over it;
    // Esc from that opening dashboard quits (there is no conversation the user
    // asked for behind it), while Enter on a row attaches to that session as
    // usual. Like `status`, it never spawns a daemon — observing requires one.
    if matches!(startup, StartupMode::Dashboard) {
        return run_dashboard(project_override, autopilot_at_start).await;
    }

    // Unified ownership (ADR-0096): every interactive session is daemon-held.
    // The default invocations — bare `neenee`, `neenee resume [id]` — are the
    // attach path now: there is no in-process harness. Bare `neenee` always
    // starts a FRESH session (`AttachAction::New`); resuming an existing one
    // is explicit — `neenee resume` picks from the daemon's sessions,
    // `neenee resume <id>` attaches to that id directly.
    if matches!(
        startup,
        StartupMode::Fresh | StartupMode::Resume(_) | StartupMode::Picker
    ) {
        let fresh = matches!(startup, StartupMode::Fresh);
        let target = match &startup {
            StartupMode::Resume(id) => id.clone(),
            // Picker (bare `resume`) attaches with no id; the daemon answers
            // Pick when several sessions exist, which the attach flow renders.
            _ => None,
        };
        return run_attached(target, fresh, project_override, autopilot_at_start, false).await;
    }

    // The in-process harness path is unreachable after the unification above;
    // it is retained only so `assemble` and its tests keep compiling while
    // the standalone driver is fully retired (tracked as ADR-0096 follow-up).
    let startup_overlay = if matches!(startup, StartupMode::Picker) {
        crate::tui::StartupOverlay::SessionsPicker
    } else {
        crate::tui::StartupOverlay::None
    };
    let boot = bootstrap::assemble(BootstrapParams {
        identity: neenee_identity(),
        principal: principal_code(),
        ui: Arc::new(crate::tui::clipboard::TuiClipboard),
        startup,
        project_root: project_override.clone(),
        autopilot: autopilot_at_start,
        single_instance,
        extra_session_tools: None,
    })
    .await?;

    let bootstrap::Bootstrap {
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
        input_history_config,
        process_lock,
        extra_session_tools: _,
        agent: _,
    } = boot;
    // The advisory process lock (ADR-0018, `--single-instance`) releases on
    // drop — hold the guard in `main`'s scope for the process lifetime.
    let _process_lock = process_lock;
    let initial_round_count = session.round_counter().await;
    let initial_commands = session.commands().await;
    // Keep a handle on this session so we can print a `neenee resume <id>`
    // hint after the TUI exits. `start_tui` moves the `Arc` into the
    // session source, so clone first.
    let session_for_exit = Arc::clone(&session);

    tokio::spawn(driver.run());

    // Start TUI in the main thread
    match start_tui(
        req_tx,
        resp_rx,
        initial_provider_name,
        initial_model_name,
        input_history,
        restored_messages,
        initial_commands,
        initial_round_count,
        custom_command_suggestions,
        tui_config,
        input_history_config,
        crate::tui::SessionSource::Local(session),
        Some(token_ledger),
        startup_overlay,
    )
    .await
    {
        Ok(outcome) => {
            // SessionEnd hooks (ADR-0025): observers fire on clean exit.
            agent_for_session_end.fire_session_end().await;
            let dedup = Config::load().input_history.dedup;
            save_history_bounded(outcome.history, dedup).await;
            // The terminal is already restored to cooked mode here, so a
            // plain `println!` renders correctly. Hint how to reopen this
            // session — but only if it actually gained content, otherwise we
            // pointlessly advertise resuming an empty conversation.
            let id = session_for_exit.id().await;
            if !session_for_exit.full_transcript().await.is_empty() {
                println!(
                    "Session {} ended. To continue it later, run: neenee resume {}",
                    short_session_id(&id),
                    id,
                );
            }
        }
        Err(e) => return Err(e),
    }

    Ok(())
}

/// Cap on how long exit is allowed to wait for the input-history write
/// before giving up (the write keeps running detached in the background).
const EXIT_HISTORY_SAVE_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(500);

/// Persist the input history off the async executor (`save_history` does
/// blocking `flock` + read-merge-write file I/O), with a bound on how long
/// exit waits for it. The TUI has already restored the terminal by now, so
/// a slow or contended write must not hold the user's shell prompt hostage:
/// after the timeout the blocking task keeps running detached and the
/// process exits anyway. The write itself is atomic (temp + rename), so an
/// interrupted process can at worst lose the merge, never corrupt the file.
async fn save_history_bounded(history: Vec<neenee_core::HistoryEntry>, dedup: bool) {
    let save = tokio::task::spawn_blocking(move || {
        Config::save_history(&history, dedup).map_err(|error| error.to_string())
    });
    match tokio::time::timeout(EXIT_HISTORY_SAVE_TIMEOUT, save).await {
        Ok(Ok(Ok(()))) => {}
        Ok(Ok(Err(error))) => {
            tracing::warn!(%error, "could not save input history on exit");
        }
        Ok(Err(join_error)) => {
            tracing::warn!(%join_error, "input-history save task failed");
        }
        Err(_) => {
            tracing::warn!(
                timeout_ms = EXIT_HISTORY_SAVE_TIMEOUT.as_millis() as u64,
                "input-history save outlived the exit bound; finishing detached"
            );
        }
    }
}

/// `neenee serve --detach`: spawn the `neenee-server` daemon in the
/// background (ADR-0096) and return immediately. If a daemon is already
/// running for this user, report it instead of spawning a second one.
fn detach_daemon() -> Result<(), String> {
    if let Some(info) = remote::discover(std::path::Path::new(".")) {
        return Err(format!(
            "a neenee daemon is already running (pid {}, port {}). Stop it before starting another.",
            info.pid, info.port
        ));
    }
    let program = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("neenee-server")))
        .filter(|path| path.is_file())
        .unwrap_or_else(|| PathBuf::from("neenee-server"));
    std::process::Command::new(&program)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("could not spawn {}: {e}", program.display()))?;
    eprintln!(
        "neenee: daemon started in the background (`neenee status` to observe, Ctrl-C it via its own terminal or kill the pid in the discovery record)"
    );
    Ok(())
}

/// `neenee dashboard` entry: open the full-screen session dashboard directly.
///
/// The dashboard's data (the live `MonitorEvent` snapshot) and its control
/// verbs (interrupt / prompt / create) ride their own daemon connections, so
/// it never depends on the attached session — but the TUI still needs one
/// hosted session as the underlying conversation carrier. We therefore attach
/// to the daemon's most-recently-active hosted session and raise the
/// dashboard over it on the first frame. Esc from that opening dashboard
/// quits (there is no conversation the user asked for behind it); Enter on a
/// row attaches to that session through the ordinary re-attach loop.
///
/// Observing is only meaningful against a running host, and a dashboard with
/// no hosted sessions has nothing to manage — so, like `neenee status`
/// (ADR-0093), a missing daemon or an empty host is a clean error rather than
/// an excuse to spawn one or fabricate a session.
async fn run_dashboard(
    project_override: Option<PathBuf>,
    autopilot_at_start: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let project_root = project_override
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let info = remote::discover(&project_root).ok_or_else(|| {
        "no neenee daemon is running. Start one with `neenee serve` \
         (or open a session first — bare `neenee` spawns one on demand)."
            .to_string()
    })?;
    // One-shot monitor snapshot to pick the carrier session: the
    // most-recently-active hosted session. Mirrored rows belong to other
    // (standalone) clients and cannot be attached to (ADR-0095).
    let mut rx = status::monitor_stream(
        &info,
        neenee_core::MonitorAction {
            watch: false,
            include_idle: true,
        },
    )
    .await
    .map_err(|e| format!("could not read the daemon's session list: {e}"))?;
    let snapshot = match rx.recv().await {
        Some(neenee_core::MonitorEvent::Snapshot(snap)) => snap,
        Some(_) => return Err("daemon monitor stream opened without a snapshot".into()),
        None => return Err("daemon closed the monitor stream".into()),
    };
    drop(rx); // one-shot: release the monitor connection before attaching
    let carrier = snapshot
        .sessions
        .iter()
        .filter(|s| s.hosting == neenee_core::SessionHosting::Hosted)
        .max_by_key(|s| s.updated_at)
        .map(|s| s.id.clone())
        .ok_or_else(|| {
            "the daemon hosts no sessions yet. Start one with bare `neenee` \
             (or `neenee attach`), then re-run `neenee dashboard`."
                .to_string()
        })?;
    run_attached(
        Some(carrier),
        false,
        project_override,
        autopilot_at_start,
        true,
    )
    .await
}

/// Attach-mode entry (`neenee attach [id]`, formerly `--attach`): find or spawn the project's
/// session server, connect over WebSocket, and drive the hosted session with
/// the ordinary TUI. This process is only a client — the server owns the
/// session lifecycle (and fires SessionEnd hooks on its own shutdown), so
/// none of that runs here.
async fn run_attached(
    session_id: Option<String>,
    fresh: bool,
    project_override: Option<PathBuf>,
    autopilot_at_start: bool,
    dashboard_entry: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let project_root = project_override
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let info = remote::ensure_server(&project_root).await?;
    let mut target = session_id.clone();
    // Only the very first connect may create a fresh session; the `/host`
    // re-attach loop below always targets an explicit existing id.
    let mut fresh_pending = fresh;
    let mut autopilot_pending = autopilot_at_start;
    // `neenee dashboard` raises the dashboard over the carrier session on the
    // first TUI entry only; a `/host` switch re-attaches into an ordinary
    // conversation view (the overlay does not re-arm).
    let mut dashboard_pending = dashboard_entry;
    // Re-attach loop: returning from the TUI with a `/host` switch target
    // re-connects to that session instead of exiting (ADR-0096).
    loop {
        let action = match &target {
            Some(id) => remote::AttachAction::Attach(Some(id.clone())),
            // Bare `neenee` asks for a brand-new session unconditionally;
            // `neenee resume` (no id) leaves the choice to the daemon
            // (auto-bind a lone session, Pick when several exist).
            None if fresh_pending => remote::AttachAction::New,
            None => remote::AttachAction::Attach(None),
        };
        fresh_pending = false;
        let handshake = remote::connect(&info, action).await?;
        let (tx, rx, hosted_session_id, round_counter, transcript, provider, model) =
            match handshake {
                remote::Handshake::Attached {
                    req_tx,
                    resp_rx,
                    session_id,
                    round_counter,
                    history,
                    provider,
                    model,
                } => (
                    req_tx,
                    resp_rx,
                    session_id,
                    round_counter,
                    history,
                    provider,
                    model,
                ),
                remote::Handshake::Pick(sessions) => {
                    eprintln!("Multiple sessions are available on the daemon:");
                    for sess in &sessions {
                        eprintln!("  {}  ({} messages)", sess.id, sess.message_count);
                    }
                    eprintln!("Re-run with a specific id: neenee attach <id>");
                    return Ok(());
                }
            };
        if autopilot_pending {
            let _ = tx.send(neenee_core::AgentRequest::SlashCommand(
                "/autopilot on".to_string(),
            ));
            autopilot_pending = false;
        }
        let input_history = Config::load_history();
        let config = Config::load();
        let tui_config = config.tui.clone();
        let input_history_config = config.input_history.clone();
        let startup_overlay = if dashboard_pending {
            dashboard_pending = false;
            crate::tui::StartupOverlay::Dashboard
        } else {
            crate::tui::StartupOverlay::None
        };
        let outcome = start_tui(
            tx,
            rx,
            // Seed the hint bar from the provider/model the daemon reported on
            // the welcome (the session's own pin when set, else the global
            // default) so the model name, reasoning effort, `@instance`, and
            // context meter render from the first frame instead of after the
            // next provider mutation.
            provider,
            model,
            input_history,
            transcript,
            Vec::new(),
            round_counter,
            vec![],
            tui_config,
            input_history_config,
            crate::tui::SessionSource::Remote {
                session_id: hosted_session_id,
            },
            None,
            startup_overlay,
        )
        .await?;
        save_history_bounded(outcome.history, config.input_history.dedup).await;
        match outcome.switch_to {
            Some(id) => {
                target = Some(id);
                continue;
            }
            None => return Ok(()),
        }
    }
}

#[cfg(test)]
mod tests;
