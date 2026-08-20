#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use neenee_persistence::config::Config;
use neenee_persistence::session;
use neenee_runtime::client;
use neenee_tui::start_tui;
mod cli;
mod commands;
mod headless;
mod identity;
mod status;

/// This CLI's identity, handed to the engine as its opening system prompt.
/// Lives here (not in `neenee-agent`) so the engine stays identity-agnostic
/// and a different frontend could reuse it as another agent.
use crate::identity::{neenee_identity, principal_code};
use cli::{CliArgs, DaemonAction, McpAction, Mode, PanelAction, SkillAction};

use std::path::PathBuf;
use std::sync::Arc;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Pre-parse for the instance-root flag (ADR-0121) so `--home` can be
    // installed before *anything* resolves a path — tracing's log dir is
    // the first consumer. A full re-parse below keeps error handling in one
    // place; this pass only trusts the flag when the command line is valid.
    install_home_override(&std::env::args().skip(1).collect::<Vec<_>>());
    let _tracing_guard = neenee_runtime::startup::init_tracing();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let result = runtime.block_on(run());
    runtime.shutdown_background();
    result
}

/// Install `--home` as the process-wide path override (ADR-0121).
///
/// Called from `main` before any `paths::get()` can cache a resolution.
/// The flag is the CLI form of the instance-root selector and wins over
/// the `NEENEE_HOME` env var; `set_default` is first-wins, so a later
/// accidental second install is a no-op. Errors are deferred to the real
/// parser in `run` — this pass stays silent so a malformed command line
/// reports exactly once.
fn install_home_override(args: &[String]) {
    let mut iter = args.iter().peekable();
    while let Some(arg) = iter.next() {
        if arg == "--home" {
            if let Some(value) = iter.next() {
                install_override(PathBuf::from(value));
            }
            return;
        }
        if let Some(value) = arg.strip_prefix("--home=").filter(|v| !v.is_empty()) {
            install_override(PathBuf::from(value));
            return;
        }
    }
    // Absent flag: record the no-op so `installed_home` and the `run`
    // consistency assertion stay well-defined.
    let _ = INSTALLED_HOME.set(None);

    fn install_override(home: PathBuf) {
        // Restate the flag as its env form for every child process: the
        // runtime's auto-spawn (`client::spawn_daemon`) and the detach path
        // build fresh command lines that cannot inherit a flag, but they do
        // inherit the environment. This makes `--home X` and
        // `NEENEE_HOME=X` indistinguishable to any descendant — the flag is
        // sugar over the env var, with identical inheritance semantics.
        //
        // SAFETY: called once from `main` before any thread exists, so no
        // concurrent reader can observe the write (setenv is not thread-safe).
        unsafe { std::env::set_var("NEENEE_HOME", &home) };
        let dirs =
            neenee_persistence::paths::Dirs::resolve(&neenee_persistence::paths::PathsOverride {
                home: Some(home.clone()),
                ..Default::default()
            });
        let _ = INSTALLED_HOME.set(Some(home));
        if let Err(previous) = neenee_persistence::paths::set_default(dirs) {
            tracing::debug!(
                previous = ?previous,
                "path override already installed; --home ignored"
            );
        }
    }
}

/// The `--home` value the pre-parser installed, if any, for the
/// consistency assertion in `run`. Kept separately from `paths::get()`
/// because the resolver layers env below the flag: `NEENEE_HOME` alone
/// also redirects every path without setting this.
static INSTALLED_HOME: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();

fn installed_home() -> Option<PathBuf> {
    INSTALLED_HOME.get().cloned().flatten()
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut parsed = match cli::parse(&std::env::args().skip(1).collect::<Vec<_>>()) {
        Ok(parsed) => parsed,
        Err(error) => {
            eprintln!("neenee: {error}\n\nRun 'neenee --help' for more information.");
            std::process::exit(2);
        }
    };

    // Stdin pipeline detection: piped input becomes (or joins) the prompt.
    // Only the prompt-bearing modes take it — piping into `neenee daemon
    // status` is a shell mistake, not a headless run.
    use std::io::{self, IsTerminal, Read};
    let stdin_input = if !io::stdin().is_terminal() {
        let mut buffer = String::new();
        if io::stdin().read_to_string(&mut buffer).is_ok() {
            let trimmed = buffer.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_string())
        } else {
            None
        }
    } else {
        None
    };
    if let Some(piped) = stdin_input {
        match &parsed.mode {
            Mode::Fresh | Mode::Run { .. } => {
                let joined = match parsed.prompt.take() {
                    Some(existing) => format!("{existing}\n\n--- Standard Input ---\n{piped}"),
                    None => piped,
                };
                parsed.prompt = Some(joined);
            }
            _ => {}
        }
    }

    // Resolve the Fresh/Run/headless intent once, here, where the terminal
    // shape is known: an explicit `-p` or a non-terminal stdout means
    // headless; `-i` forces the TUI; `run` is headless by definition.
    if matches!(parsed.mode, Mode::Fresh) && !parsed.interactive {
        let stdout_is_tty = io::stdout().is_terminal();
        if parsed.prompt_from_flag || parsed.json || (parsed.prompt.is_some() && !stdout_is_tty) {
            parsed.mode = Mode::Run {
                prompt: parsed.prompt.clone().unwrap_or_default(),
            };
        }
    }

    let CliArgs {
        mode,
        project: project_override,
        autopilot: autopilot_at_start,
        interactive,
        prompt,
        json: _,
        remote,
        token,
        home,
        ..
    } = parsed;

    // The pre-parser in `main` already installed the override; assert the
    // two passes agree so the flag can never parse one way and install
    // another (ADR-0121's single-source rule for path-layer flags).
    debug_assert_eq!(home, installed_home());

    match mode {
        Mode::Version => {
            println!("neenee {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Mode::Help(topic) => {
            if let Some(text) = cli::help_text(topic.as_deref()) {
                print!("{text}");
            }
            Ok(())
        }
        Mode::Completions(shell) => {
            print!("{}", cli::completion_script(shell));
            Ok(())
        }
        #[cfg(debug_assertions)]
        Mode::Showcase(component) => neenee_tui::showcase::run(&component),
        Mode::Doctor => session::run_doctor(project_override.as_deref())
            .await
            .map_err(Into::into),
        Mode::Config(action) => {
            // Standalone config/auth commands read the instance store; run the
            // one-shot legacy migration first so a pre-refactor install is
            // converted before anything lists or edits instances.
            neenee_agent::catalog::migrate_legacy_state();
            commands::config::run(action)
        }
        Mode::Auth(action) => {
            neenee_agent::catalog::migrate_legacy_state();
            commands::auth::run(action)
        }
        Mode::Mcp(McpAction::List) => commands::mcp::run(),
        Mode::Skill(SkillAction::List) => commands::skill::run().await,
        Mode::Session(action) => commands::session::run(action, project_override).await,
        Mode::Daemon(action) => run_daemon_action(action, project_override).await,
        Mode::Panel(action) => run_panel(action),
        Mode::Dashboard => run_dashboard(project_override, autopilot_at_start).await,
        Mode::Attach { id } => {
            run_attached(id, false, project_override, autopilot_at_start, false, None).await
        }
        Mode::Run { prompt } => {
            if interactive {
                // `run -i` deliberately switches to the TUI with the prompt.
                run_attached(
                    None,
                    true,
                    project_override,
                    autopilot_at_start,
                    false,
                    Some(prompt),
                )
                .await
            } else {
                headless::run_headless(
                    prompt,
                    parsed.json,
                    project_override,
                    autopilot_at_start,
                    remote,
                    token,
                )
                .await
            }
        }
        Mode::Fresh => {
            run_attached(
                None,
                true,
                project_override,
                autopilot_at_start,
                false,
                prompt,
            )
            .await
        }
    }
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

/// `neenee daemon start` (detached, the default): spawn the daemon in the
/// background and return. If a daemon is already running, report it
/// instead of spawning a second one.
fn detach_daemon(flags: &DaemonStart) -> Result<(), String> {
    if let Some(info) = client::discover(std::path::Path::new(".")) {
        return Err(format!(
            "a neenee daemon is already running (pid {}, port {}). Stop it with `neenee stop` before starting another.",
            info.pid, info.port
        ));
    }
    let program = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("neenee"));
    let mut command = std::process::Command::new(&program);
    // The supervisor form: the child re-enters `daemon start --fg` —
    // foreground by construction, its lifecycle flags from [daemon] config.
    // No `--home` restatement needed: `install_home_override` restates the
    // flag as `NEENEE_HOME` in this process's environment, and the child
    // inherits it (ADR-0121).
    command.args(["daemon", "start", "--fg"]);
    // Every explicit start flag survives the detach: the child is the same
    // start the operator asked for, minus the daemonization. Dropping them
    // here would make `daemon start --port N` silently bind the default —
    // exactly the class of lie a detached process can afford (nobody is
    // watching its output). Only pass what was set: unset flags keep the
    // [daemon]-config defaults the child resolves itself.
    if let Some(port) = flags.port {
        command.arg("--port").arg(port.to_string());
    }
    if flags.public {
        command.arg("--public");
    }
    if flags.no_local_auth {
        command.arg("--no-local-auth");
    }
    if let Some(minutes) = flags.idle_exit_minutes {
        command.arg("--idle-exit").arg(minutes.to_string());
    }
    if let Some(secs) = flags.shutdown_grace_secs {
        command.arg("--grace").arg(secs.to_string());
    }
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
/// `neenee panel url` / `neenee panel open` (ADR-0105): the web panel's
/// address for the running daemon — with the bearer token as a query
/// param when the daemon requires one. The token is only ever printed on
/// this explicit, operator-initiated request (never in daemon logs or
/// banners); the panel persists it to localStorage on first visit.
///
/// `url` prints (scripts, remote forwarding); `open` additionally hands
/// the URL to the platform browser (`$BROWSER`, else xdg-open / open).
/// The bare `neenee panel` prints — it was the verb's whole meaning
/// before the subcommands existed, and a bare noun that opens a GUI is a
/// surprise on headless boxes.
fn panel_url(info: &client::DaemonInfo) -> String {
    let mut url = format!("http://127.0.0.1:{}", info.port);
    if let Some(token) = &info.token {
        url.push_str(&format!("/?token={token}"));
    }
    url
}

fn run_panel(action: PanelAction) -> Result<(), Box<dyn std::error::Error>> {
    let info = match client::discover(std::path::Path::new(".")) {
        Some(info) => info,
        None => {
            return Err(
                "no neenee daemon is running (start one with `neenee daemon start`)".into(),
            );
        }
    };
    let url = panel_url(&info);
    match action {
        PanelAction::Url => println!("{url}"),
        PanelAction::Open => {
            println!("{url}");
            // Best-effort: a missing browser opener is a note, not an
            // error — the URL is already on stdout for copy-paste.
            let program = std::env::var("BROWSER").ok().filter(|b| !b.is_empty());
            let (program, args): (String, Vec<String>) = match program {
                Some(b) => (b, vec![url.clone()]),
                None if cfg!(target_os = "macos") => ("open".to_string(), vec![url.clone()]),
                None => ("xdg-open".to_string(), vec![url.clone()]),
            };
            match std::process::Command::new(&program)
                .args(&args)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
            {
                Ok(_) => {}
                Err(e) => eprintln!("neenee: could not launch {program} ({e}); open {url}"),
            }
        }
    }
    Ok(())
}

/// `neenee daemon stop` (ADR-0100/0116): stop the running daemon through
/// the budget-aware shutdown pipeline (graceful control verb → SIGTERM →
/// SIGKILL). Stopping a daemon that is not running (or whose record is
/// stale) is a success — the operator's desired end state ("no daemon")
/// is already true.
async fn stop_daemon() -> Result<(), String> {
    let info = match client::discover(std::path::Path::new(".")) {
        Some(info) => info,
        None => {
            let lock_path = neenee_runtime::serve_discovery::global_lock_path();
            if let Some(pid) = neenee_persistence::lock::ProcessLock::probe_holder(&lock_path) {
                if client::is_process_alive(pid) {
                    client::DaemonInfo {
                        pid,
                        port: neenee_runtime::startup::env_default_port(),
                        token: None,
                        project_root: String::new(),
                        started_at: 0,
                        #[cfg(unix)]
                        uds_path: Some(neenee_runtime::serve_discovery::default_uds_path()),
                        #[cfg(not(unix))]
                        uds_path: None,
                        version: None,
                        grace_secs: None,
                    }
                } else {
                    eprintln!("neenee: no daemon is running.");
                    return Ok(());
                }
            } else {
                eprintln!("neenee: no daemon is running.");
                return Ok(());
            }
        }
    };
    client::stop(&info).await?;
    eprintln!("neenee: daemon stopped (pid {}).", info.pid);
    Ok(())
}
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

/// `neenee daemon <action>` dispatch (ADR-0116: the daemon noun owns
/// start/stop/status; the retired top-level spellings route here too).
async fn run_daemon_action(
    action: DaemonAction,
    project_override: Option<PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    match action {
        DaemonAction::Start {
            foreground,
            port,
            public,
            no_local_auth,
            idle_exit_minutes,
            shutdown_grace_secs,
        } => {
            if !foreground {
                return detach_daemon(&DaemonStart {
                    port,
                    public,
                    no_local_auth,
                    idle_exit_minutes,
                    shutdown_grace_secs,
                })
                .map_err(Into::into);
            }
            run_daemon_foreground(DaemonStart {
                port,
                public,
                no_local_auth,
                idle_exit_minutes,
                shutdown_grace_secs,
            })
            .await
        }
        DaemonAction::Stop => stop_daemon().await.map_err(Into::into),
        DaemonAction::Status {
            watch,
            json,
            include_idle,
            diagnostic,
        } => {
            let project_root = project_override
                .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
            status::run(
                &project_root,
                status::StatusOptions {
                    watch,
                    json,
                    include_idle,
                    diagnostic,
                },
            )
            .await
            .map_err(Into::into)
        }
    }
}

/// The daemon-start flags that reach the runtime (one struct, one place).
struct DaemonStart {
    port: Option<u16>,
    public: bool,
    no_local_auth: bool,
    idle_exit_minutes: Option<u64>,
    shutdown_grace_secs: Option<u64>,
}

/// `neenee daemon start`: detached unless `--fg`. Detaching is the default
/// because the user asked for a *daemon*; `--fg` is the supervisor shape
/// (systemd/tmux foreground processes).
async fn run_daemon_foreground(flags: DaemonStart) -> Result<(), Box<dyn std::error::Error>> {
    let mut lifecycle = neenee_runtime::host::LifecycleOptions::from_config();
    if let Some(minutes) = flags.idle_exit_minutes {
        lifecycle.idle_exit = match minutes {
            0 => None,
            m => Some(std::time::Duration::from_secs(m * 60)),
        };
    }
    if let Some(secs) = flags.shutdown_grace_secs {
        lifecycle.shutdown_grace = std::time::Duration::from_secs(secs.max(1));
    }
    // An explicitly requested port must fail loudly when taken; only the
    // default port falls back to ephemeral (ADR-0105). The *default* itself
    // honours NEENEE_PORT so an isolated instance (ADR-0121) takes its own
    // port instead of contending with the host daemon on 9800.
    let port = flags
        .port
        .unwrap_or(neenee_runtime::startup::env_default_port());
    let outcome = neenee_runtime::host::run_with_gate(
        neenee_runtime::host::HostIdentity {
            identity: neenee_identity(),
            principal: principal_code(),
            ui: Arc::new(neenee_tui::clipboard::TuiClipboard),
        },
        neenee_runtime::host::HostOptions {
            port,
            expose: if flags.public {
                neenee_runtime::serve::ServeExpose::Public
            } else {
                neenee_runtime::serve::ServeExpose::Local
            },
            token: None,
            // CLI flag wins over config; both default to the secure
            // posture (loopback token on, ADR-0105).
            local_auth: !flags.no_local_auth
                && neenee_persistence::config::Config::load().daemon.local_auth,
            port_fallback: flags.port.is_none(),
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
                "neenee: daemon stopped ({reason}); grace budget expired, stragglers were \
                 aborted — see the log."
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
    // `neenee attach` with no id picks interactively (ADR-0116): the first
    // connect opens the TUI sessions picker over a throwaway carrier; the
    // picker's `/sessions <id>` exit re-attaches through `switch_to`.
    let mut pick_pending = session_id.is_none() && !fresh;
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
            // Bare `neenee` asks for a brand-new session unconditionally.
            None if fresh_pending => client::AttachAction::New,
            // `neenee attach` with no id opens the TUI picker (ADR-0116).
            None if pick_pending => client::AttachAction::Picker,
            // Auto-bind a lone session (the daemon decides; several mean
            // the picker, which the Pick fallback below turns interactive).
            None => client::AttachAction::Attach(None),
        };
        fresh_pending = false;
        pick_pending = false;
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
                // A daemon that answers `Pick` wants the user to choose.
                // Choosing is interactive (ADR-0116): reconnect as a picker
                // carrier and let the TUI modal do the listing, with fuzzy
                // filter, detail pane, and Enter-to-open — not a printed
                // stderr list that makes the user copy an id by hand.
                client::Handshake::Pick(_) => {
                    let handshake = client::connect(&info, client::AttachAction::Picker).await?;
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
                        client::Handshake::Pick(_) => {
                            return Err("the daemon offered no session to pick from".into());
                        }
                    }
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
