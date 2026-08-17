#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use neenee_persistence::config::Config;
use neenee_persistence::session;
use neenee_runtime::client;
use neenee_tui::start_tui;
mod commands;
mod headless;
mod identity;
mod status;

/// This CLI's identity, handed to the engine as its opening system prompt.
/// Lives here (not in `neenee-agent`) so the engine stays identity-agnostic
/// and a different frontend could reuse it as another agent.
use crate::identity::{neenee_identity, principal_code};
use neenee_runtime::startup::{
    CliArgs, DaemonAction, StartupMode, completion_script, help_text, init_tracing, parse_args,
};

use std::path::PathBuf;
use std::sync::Arc;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _tracing_guard = init_tracing();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let result = runtime.block_on(run());
    runtime.shutdown_background();
    result
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let CliArgs {
        mut mode,
        project: project_override,
        autopilot: autopilot_at_start,
        single_instance: _,
        interactive,
        remote,
        token,
    } = match parse_args(std::env::args().skip(1).collect()) {
        Ok(parsed) => parsed,
        Err(error) => {
            eprintln!("neenee: {error}\n\nRun 'neenee --help' for more information.");
            std::process::exit(2);
        }
    };

    // Stdin pipeline detection: if stdin is piped, attach content to prompt
    use std::io::{self, IsTerminal, Read};
    let stdin_input = if !io::stdin().is_terminal() {
        let mut buffer = String::new();
        if io::stdin().read_to_string(&mut buffer).is_ok() {
            let trimmed = buffer.trim();
            if !trimmed.is_empty() {
                Some(trimmed.to_string())
            } else {
                None
            }
        } else {
            None
        }
    } else {
        None
    };

    if let Some(piped) = stdin_input {
        match mode {
            StartupMode::Fresh => {
                if interactive {
                    mode = StartupMode::FreshWithPrompt(piped);
                } else {
                    mode = StartupMode::Headless {
                        prompt: piped,
                        json: false,
                    };
                }
            }
            StartupMode::FreshWithPrompt(p) => {
                let combined = format!("{p}\n\n--- Standard Input ---\n{piped}");
                if interactive {
                    mode = StartupMode::FreshWithPrompt(combined);
                } else {
                    mode = StartupMode::Headless {
                        prompt: combined,
                        json: false,
                    };
                }
            }
            StartupMode::Headless { prompt, json } => {
                let combined = format!("{prompt}\n\n--- Standard Input ---\n{piped}");
                mode = StartupMode::Headless {
                    prompt: combined,
                    json,
                };
            }
            _ => {}
        }
    }

    if matches!(mode, StartupMode::Version) {
        println!("neenee {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    if let StartupMode::Help(topic) = &mode
        && let Some(text) = help_text(topic.as_deref())
    {
        print!("{text}");
        return Ok(());
    }

    if let StartupMode::Completions(shell) = &mode
        && let Some(script) = completion_script(shell)
    {
        print!("{script}");
        return Ok(());
    }

    #[cfg(debug_assertions)]
    if let StartupMode::Showcase(component) = &mode {
        return neenee_tui::showcase::run(component);
    }

    if matches!(mode, StartupMode::Doctor) {
        session::run_doctor(project_override.as_deref()).await?;
        return Ok(());
    }

    // Subcommands: config, auth, mcp, skill, session
    if let StartupMode::Config(action) = mode {
        return commands::config::run(action);
    }

    if let StartupMode::Auth(action) = mode {
        return commands::auth::run(action);
    }

    if let StartupMode::Mcp(action) = mode {
        return commands::mcp::run(action);
    }

    if let StartupMode::Skill(action) = mode {
        return commands::skill::run(action).await;
    }

    if let StartupMode::Session(action) = mode {
        return commands::session::run(action, project_override, autopilot_at_start).await;
    }

    // Daemon management (new subcommand & legacy serve/stop/status)
    if let StartupMode::Daemon(action) = mode {
        match action {
            DaemonAction::Start {
                port,
                public,
                detach,
                idle_exit_minutes,
                shutdown_grace_secs,
                no_local_auth,
                port_explicit,
            } => {
                return run_serve(
                    port,
                    public,
                    detach,
                    idle_exit_minutes,
                    shutdown_grace_secs,
                    no_local_auth,
                    port_explicit,
                )
                .await;
            }
            DaemonAction::Stop => {
                return stop_daemon().await.map_err(Into::into);
            }
            DaemonAction::Status {
                watch,
                json,
                include_idle,
            } => {
                let project_root = project_override.clone().unwrap_or_else(|| {
                    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
                });
                return status::run(
                    &project_root,
                    status::StatusOptions {
                        watch,
                        json,
                        include_idle,
                    },
                )
                .await
                .map_err(Into::into);
            }
        }
    }

    // Headless execution mode
    if let StartupMode::Headless { prompt, json } = mode {
        return headless::run_headless(
            prompt,
            json,
            project_override,
            autopilot_at_start,
            remote,
            token,
        )
        .await;
    }

    // Interactive session with initial prompt
    if let StartupMode::FreshWithPrompt(prompt) = mode {
        return run_attached(
            None,
            true,
            project_override,
            autopilot_at_start,
            false,
            Some(prompt),
        )
        .await;
    }

    // Attach mode
    if let StartupMode::Attach(session_id) = &mode {
        return run_attached(
            session_id.clone(),
            false,
            project_override,
            autopilot_at_start,
            false,
            None,
        )
        .await;
    }

    if matches!(mode, StartupMode::Stop) {
        return stop_daemon().await.map_err(Into::into);
    }

    if matches!(mode, StartupMode::Panel) {
        return print_panel_url();
    }

    if let StartupMode::Serve {
        port,
        public,
        detach,
        idle_exit_minutes,
        shutdown_grace_secs,
        no_local_auth,
        port_explicit,
    } = &mode
    {
        return run_serve(
            *port,
            *public,
            *detach,
            *idle_exit_minutes,
            *shutdown_grace_secs,
            *no_local_auth,
            *port_explicit,
        )
        .await;
    }

    if let StartupMode::Status {
        watch,
        json,
        include_idle,
    } = &mode
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

    if matches!(mode, StartupMode::Dashboard) {
        return run_dashboard(project_override, autopilot_at_start).await;
    }

    if matches!(
        mode,
        StartupMode::Fresh | StartupMode::Resume(_) | StartupMode::Picker
    ) {
        let fresh = matches!(mode, StartupMode::Fresh);
        let target = match &mode {
            StartupMode::Resume(id) => id.clone(),
            _ => None,
        };
        return run_attached(
            target,
            fresh,
            project_override,
            autopilot_at_start,
            false,
            None,
        )
        .await;
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
async fn save_history_bounded(history: Vec<neenee_contracts::HistoryEntry>, dedup: bool) {
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

/// `neenee serve --detach`: spawn the daemon in the
/// background and return immediately. If a daemon is already
/// running for this user, report it instead of spawning a second one.
fn detach_daemon() -> Result<(), String> {
    if let Some(info) = client::discover(std::path::Path::new(".")) {
        return Err(format!(
            "a neenee daemon is already running (pid {}, port {}). Stop it with `neenee stop` before starting another.",
            info.pid, info.port
        ));
    }
    let program = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("neenee"));
    let mut command = std::process::Command::new(&program);
    command.arg("serve");
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    // Own process group (ADR-0101): a detached daemon must not sit in the
    // invoking shell's foreground process group, or the terminal's Ctrl-C
    // SIGINTs it along with everything else — the exact opposite of what
    // "detach" promises.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        command.process_group(0);
    }
    command
        .spawn()
        .map_err(|e| format!("could not spawn {}: {e}", program.display()))?;
    eprintln!(
        "neenee: daemon started in the background (`neenee status` to observe, `neenee stop` to stop it)"
    );
    Ok(())
}

/// `neenee panel` (ADR-0105): print the web panel URL for the running
/// daemon — with the bearer token as a query param when the daemon requires
/// one. The token is only ever printed on this explicit, operator-initiated
/// request (never in daemon logs or banners); the panel persists it to
/// localStorage on first visit.
fn print_panel_url() -> Result<(), Box<dyn std::error::Error>> {
    match client::discover(std::path::Path::new(".")) {
        Some(info) => {
            let mut url = format!("http://127.0.0.1:{}", info.port);
            if let Some(token) = &info.token {
                url.push_str(&format!("/?token={token}"));
            }
            println!("{url}");
            Ok(())
        }
        None => Err("no neenee daemon is running (start one with `neenee daemon start`)".into()),
    }
}

/// `neenee stop` (ADR-0100): stop the running daemon through the tiered
/// shutdown pipeline (graceful control verb -> OS SIGTERM -> SIGKILL).
/// Stopping a daemon that is not running (or whose record is stale) is a
/// success — the operator's desired end state ("no daemon") is already true.
async fn stop_daemon() -> Result<(), String> {
    let Some(info) = client::discover(std::path::Path::new(".")) else {
        eprintln!("neenee: no daemon is running.");
        return Ok(());
    };
    client::stop(&info).await?;
    eprintln!("neenee: daemon stopped (pid {}).", info.pid);
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
    let info = client::discover(&project_root).ok_or_else(|| {
        "no neenee daemon is running. Start one with `neenee serve` \
         (or open a session first — bare `neenee` spawns one on demand)."
            .to_string()
    })?;
    if !client::versions_compatible(&info) {
        return Err(client::version_mismatch(&info).into());
    }
    // One-shot monitor snapshot to pick the carrier session: the
    // most-recently-active hosted session (ADR-0096: every row is hosted).
    let mut rx = client::monitor_stream(
        &info,
        neenee_contracts::MonitorAction {
            watch: false,
            include_idle: true,
        },
    )
    .await
    .map_err(|e| format!("could not read the daemon's session list: {e}"))?;
    let snapshot = match rx.recv().await {
        Some(neenee_contracts::MonitorEvent::Snapshot(snap)) => snap,
        Some(_) => return Err("daemon monitor stream opened without a snapshot".into()),
        None => return Err("daemon closed the monitor stream".into()),
    };
    drop(rx); // one-shot: release the monitor connection before attaching
    let carrier = snapshot
        .sessions
        .iter()
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
        None,
    )
    .await
}

async fn run_serve(
    port: u16,
    public: bool,
    detach: bool,
    idle_exit_minutes: Option<u64>,
    shutdown_grace_secs: Option<u64>,
    no_local_auth: bool,
    port_explicit: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if detach {
        return detach_daemon().map_err(Into::into);
    }
    let mut lifecycle = neenee_runtime::host::LifecycleOptions::from_config();
    if let Some(minutes) = idle_exit_minutes {
        lifecycle.idle_exit = match minutes {
            0 => None,
            m => Some(std::time::Duration::from_secs(m * 60)),
        };
    }
    if let Some(secs) = shutdown_grace_secs {
        lifecycle.shutdown_grace = std::time::Duration::from_secs(secs.max(1));
    }
    let outcome = neenee_runtime::host::run_with_gate(
        neenee_runtime::host::HostIdentity {
            identity: neenee_identity(),
            principal: principal_code(),
            ui: Arc::new(neenee_tui::clipboard::TuiClipboard),
        },
        neenee_runtime::host::HostOptions {
            port,
            expose: if public {
                neenee_runtime::serve::ServeExpose::Public
            } else {
                neenee_runtime::serve::ServeExpose::Local
            },
            token: None,
            // CLI flag wins over config; both default to the secure posture
            // (loopback token on, ADR-0105).
            local_auth: !no_local_auth
                && neenee_persistence::config::Config::load().daemon.local_auth,
            // An explicitly requested port must fail loudly when taken; only
            // the default port falls back to ephemeral (ADR-0105).
            port_fallback: !port_explicit,
            #[cfg(unix)]
            uds_path: Some(neenee_runtime::serve_discovery::default_uds_path()),
        },
        std::sync::Arc::new(neenee_runtime::shutdown::ShutdownGate::new()),
        lifecycle,
    )
    .await;
    match &outcome {
        neenee_runtime::host::RunOutcome::Stopped { reason } => {
            eprintln!("neenee: daemon stopped ({reason}).");
        }
        neenee_runtime::host::RunOutcome::ForcedExit { reason } => {
            eprintln!(
                "neenee: daemon stopped ({reason}); grace budget expired, stragglers were aborted — see the log."
            );
        }
        neenee_runtime::host::RunOutcome::StartupFailed(what) => {
            eprintln!("neenee: {what}");
        }
    }
    std::process::exit(outcome.exit_code());
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
    mut initial_prompt: Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let project_root = project_override
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let info = client::ensure_daemon(&project_root).await?;
    // Version skew (ADR-0100 rule 4): a daemon from another build speaks a
    // wire protocol this client may not share. Fail loud with the fix
    // rather than mis-serializing frames mid-session.
    if !client::versions_compatible(&info) {
        return Err(client::version_mismatch(&info).into());
    }
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
            Some(id) => client::AttachAction::Attach(Some(id.clone())),
            // Bare `neenee` asks for a brand-new session unconditionally;
            // `neenee resume` (no id) leaves the choice to the daemon
            // (auto-bind a lone session, Pick when several exist).
            None if fresh_pending => client::AttachAction::New,
            None => client::AttachAction::Attach(None),
        };
        fresh_pending = false;
        let handshake = client::connect(&info, action).await?;
        let (tx, rx, hosted_session_id, round_counter, transcript, provider, model) =
            match handshake {
                client::Handshake::Attached {
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
                client::Handshake::Pick(sessions) => {
                    eprintln!("Multiple sessions are available on the daemon:");
                    for sess in &sessions {
                        eprintln!("  {}  ({} messages)", sess.id, sess.message_count);
                    }
                    eprintln!("Re-run with a specific id: neenee attach <id>");
                    return Ok(());
                }
            };
        if autopilot_pending {
            let _ = tx.send(neenee_contracts::AgentRequest::SlashCommand(
                "/autopilot on".to_string(),
            ));
            autopilot_pending = false;
        }
        if let Some(prompt) = initial_prompt.take() {
            let _ = tx.send(neenee_contracts::AgentRequest::Chat {
                text: prompt,
                images: Vec::new(),
                sent_at_ms: None,
            });
        }
        let input_history = Config::load_history();
        let config = Config::load();
        let tui_config = config.tui.clone();
        let input_history_config = config.input_history.clone();
        let startup_overlay = if dashboard_pending {
            dashboard_pending = false;
            neenee_tui::StartupOverlay::Dashboard
        } else {
            neenee_tui::StartupOverlay::None
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
            neenee_tui::SessionSource::Remote {
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
