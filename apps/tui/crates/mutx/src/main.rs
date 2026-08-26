#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use muta_runtime::client;
use mutx::start_tui;
mod cli;
mod headless;
use cli::{CliArgs, Mode};

use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Pre-parse for the instance-root flag (ADR-0121) so `--home` can be
    // installed before *anything* resolves a path — tracing's log dir is
    // the first consumer. A full re-parse below keeps error handling in one
    // place; this pass only trusts the flag when the command line is valid.
    install_home_override(&std::env::args().skip(1).collect::<Vec<_>>());
    let _tracing_guard = muta_runtime::startup::init_tracing();
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
/// the `MUTA_HOME` env var; `set_default` is first-wins, so a later
/// accidental second install is a no-op. Errors are deferred to the real
/// parser in `run` — this pass stays silent so a malformed command line
/// reports exactly once.
fn install_home_override(args: &[String]) {
    // `--home` plus the per-category flags (ADR-0014 §3 tier 1) resolve in
    // the same one-time pre-parse; `PathsOverride` defines their precedence
    // (a category-specific flag wins over the instance root for its own
    // category only).
    let mut config_dir: Option<PathBuf> = None;
    let mut data_dir: Option<PathBuf> = None;
    let mut state_dir: Option<PathBuf> = None;
    let mut cache_dir: Option<PathBuf> = None;
    let mut home: Option<PathBuf> = None;
    let mut iter = args.iter().peekable();
    while let Some(arg) = iter.next() {
        let mut next_value = |flag: &str| -> Option<PathBuf> {
            if let Some(value) = iter.next() {
                return Some(PathBuf::from(value));
            }
            let _ = flag;
            None
        };
        match arg.as_str() {
            "--home" => home = next_value("--home").or(home),
            "--config-dir" => config_dir = next_value("--config-dir").or(config_dir),
            "--data-dir" => data_dir = next_value("--data-dir").or(data_dir),
            "--state-dir" => state_dir = next_value("--state-dir").or(state_dir),
            "--cache-dir" => cache_dir = next_value("--cache-dir").or(cache_dir),
            _ => {}
        }
        if let Some(value) = arg.strip_prefix("--home=").filter(|v| !v.is_empty()) {
            home = Some(PathBuf::from(value));
        }
        for (flag, slot) in [
            ("--config-dir=", &mut config_dir),
            ("--data-dir=", &mut data_dir),
            ("--state-dir=", &mut state_dir),
            ("--cache-dir=", &mut cache_dir),
        ] {
            if let Some(value) = arg.strip_prefix(flag).filter(|v| !v.is_empty()) {
                *slot = Some(PathBuf::from(value));
            }
        }
    }
    let any = home.is_some()
        || config_dir.is_some()
        || data_dir.is_some()
        || state_dir.is_some()
        || cache_dir.is_some();
    if any {
        install_override(home, config_dir, data_dir, state_dir, cache_dir);
        return;
    }
    // Absent flag: record the no-op so `installed_home` and the `run`
    // consistency assertion stay well-defined.
    let _ = INSTALLED_HOME.set(None);

    fn install_override(
        home: Option<PathBuf>,
        config_dir: Option<PathBuf>,
        data_dir: Option<PathBuf>,
        state_dir: Option<PathBuf>,
        cache_dir: Option<PathBuf>,
    ) {
        // Restate the flags as their env forms for every child process: the
        // runtime's auto-spawn (`client::spawn_daemon`) and the detach path
        // build fresh command lines that cannot inherit a flag, but they do
        // inherit the environment. This makes `--home X` and
        // `MUTA_HOME=X` (and each `--*-dir` with its `MUTA_*_DIR`)
        // indistinguishable to any descendant — the flags are sugar over
        // the env vars, with identical inheritance semantics.
        //
        // SAFETY: called once from `main` before any thread exists, so no
        // concurrent reader can observe the write (setenv is not thread-safe).
        if let Some(home) = &home {
            unsafe { std::env::set_var("MUTA_HOME", home) };
        }
        for (dir, var) in [
            (&config_dir, "MUTA_CONFIG_DIR"),
            (&data_dir, "MUTA_DATA_DIR"),
            (&state_dir, "MUTA_STATE_DIR"),
            (&cache_dir, "MUTA_CACHE_DIR"),
        ] {
            if let Some(dir) = dir {
                unsafe { std::env::set_var(var, dir) };
            }
        }
        let _ = INSTALLED_HOME.set(home.clone());
        let dirs =
            muta_persistence::paths::Dirs::resolve(&muta_persistence::paths::PathsOverride {
                home,
                config_dir,
                data_dir,
                state_dir,
                cache_dir,
            });
        if let Err(previous) = muta_persistence::paths::set_default(dirs) {
            tracing::debug!(
                previous = ?previous,
                "path override already installed; --home ignored"
            );
        }
    }
}

/// The `--home` value the pre-parser installed, if any, for the
/// consistency assertion in `run`. Kept separately from `paths::get()`
/// because the resolver layers env below the flag: `MUTA_HOME` alone
/// also redirects every path without setting this.
static INSTALLED_HOME: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();

fn installed_home() -> Option<PathBuf> {
    INSTALLED_HOME.get().cloned().flatten()
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut parsed = match cli::parse(&std::env::args().skip(1).collect::<Vec<_>>()) {
        Ok(parsed) => parsed,
        Err(error) => {
            eprintln!("mutx: {error}\n\nRun 'mutx --help' for more information.");
            std::process::exit(2);
        }
    };

    // Stdin pipeline detection: piped input becomes (or joins) the prompt.
    // Only the prompt-bearing modes take it — piping into a non-prompt
    // command is a shell mistake, not a headless run.
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
        yolo: yolo_at_start,
        interactive,
        prompt,
        json: _,
        remote,
        token,
        home,
        config_dir,
        data_dir,
        state_dir,
        cache_dir,
        ..
    } = parsed;

    // The pre-parser in `main` already installed the override; assert the
    // two passes agree so the flag can never parse one way and install
    // another (ADR-0121's single-source rule for path-layer flags). The
    // per-category flags are consumed by the same pre-parser; binding them
    // here keeps the two parses honest about all five selectors.
    debug_assert_eq!(home, installed_home());
    debug_assert!(
        config_dir.is_none() && data_dir.is_none() && state_dir.is_none() && cache_dir.is_none()
            || std::env::var_os("MUTA_CONFIG_DIR").is_some()
            || std::env::var_os("MUTA_DATA_DIR").is_some()
            || std::env::var_os("MUTA_STATE_DIR").is_some()
            || std::env::var_os("MUTA_CACHE_DIR").is_some()
    );

    match mode {
        Mode::Version => {
            println!("mutx {}", env!("CARGO_PKG_VERSION"));
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
        Mode::Showcase(component) => mutx::showcase::run(&component),
        Mode::Dashboard => run_dashboard(project_override, yolo_at_start).await,
        Mode::Attach { id } => {
            run_attached(id, false, project_override, yolo_at_start, false, None).await
        }
        Mode::Run { prompt } => {
            if interactive {
                // `run -i` deliberately switches to the TUI with the prompt.
                run_attached(
                    None,
                    true,
                    project_override,
                    yolo_at_start,
                    false,
                    Some(prompt),
                )
                .await
            } else {
                headless::run_headless(
                    prompt,
                    parsed.json,
                    project_override,
                    yolo_at_start,
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
                yolo_at_start,
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
async fn save_history_bounded(history: Vec<muta_contracts::HistoryEntry>, dedup: bool) {
    let save = tokio::task::spawn_blocking(move || {
        mutx::config::save_history(&history, dedup).map_err(|error| error.to_string())
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

/// The dashboard's data (the live `MonitorEvent` snapshot) and its control
/// verbs (interrupt / prompt / create) ride their own daemon connections, so
/// it never depends on the attached session — but the TUI still needs one
/// hosted session as the underlying conversation carrier. We therefore attach
/// to the daemon's most-recently-active hosted session and raise the
/// dashboard over it on the first frame. Leaving that opening dashboard
/// quits the whole TUI (Esc immediately; Ctrl+C via the app-wide
/// double-press) — there is no conversation the user asked for behind it.
/// Enter on a row attaches to that session through the ordinary re-attach
/// loop.
///
/// Observing is only meaningful against a running host, and a dashboard with
/// no hosted sessions has nothing to manage. Starting `mutx dashboard` still
/// performs the normal Muta daemon readiness check, but it does not fabricate
/// a carrier session just to display an empty dashboard.
async fn run_dashboard(
    project_override: Option<PathBuf>,
    yolo_at_start: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let project_root = project_override
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let info = client::ensure_daemon(&project_root).await?;
    if !client::versions_compatible(&info) {
        return Err(client::incompatibility_error(&info).into());
    }
    // One-shot monitor snapshot to pick the carrier session: the
    // most-recently-active hosted session (ADR-0096: every row is hosted).
    let mut rx = client::monitor_stream(
        &info,
        muta_contracts::MonitorAction {
            watch: false,
            include_idle: true,
        },
    )
    .await
    .map_err(|e| format!("could not read the daemon's session list: {e}"))?;
    let snapshot = match rx.recv().await {
        Some(muta_contracts::MonitorEvent::Snapshot(snap)) => snap,
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
            "the daemon hosts no sessions yet. Start one with bare `mutx`, \
             then re-run `mutx dashboard`."
                .to_string()
        })?;
    run_attached(
        Some(carrier),
        false,
        project_override,
        yolo_at_start,
        true,
        None,
    )
    .await
}

/// Attach-mode entry (`mutx attach [id]`, formerly `--attach`): find or spawn the project's
/// session server, connect over WebSocket, and drive the hosted session with
/// the ordinary TUI. This process is only a client — the server owns the
/// session lifecycle (and fires SessionEnd hooks on its own shutdown), so
/// none of that runs here.
async fn run_attached(
    session_id: Option<String>,
    fresh: bool,
    project_override: Option<PathBuf>,
    yolo_at_start: bool,
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
        return Err(client::incompatibility_error(&info).into());
    }
    let mut target = session_id.clone();
    // Only the very first connect may create a fresh session; the `/host`
    // re-attach loop below always targets an explicit existing id.
    let mut fresh_pending = fresh;
    // `mutx attach` with no id picks interactively (ADR-0116): the first
    // connect opens the TUI sessions picker over a throwaway carrier; the
    // picker's `/sessions <id>` exit re-attaches through `switch_to`.
    let mut pick_pending = session_id.is_none() && !fresh;
    let mut yolo_pending = yolo_at_start;
    // `mutx dashboard` raises the dashboard over the carrier session on the
    // first TUI entry only; a `/host` switch re-attaches into an ordinary
    // conversation view (the overlay does not re-arm).
    let mut dashboard_pending = dashboard_entry;
    // Re-attach loop: returning from the TUI with a `/host` switch target
    // re-connects to that session instead of exiting (ADR-0096).
    loop {
        let action = match &target {
            Some(id) => client::AttachAction::Attach(Some(id.clone())),
            // Bare `mutx` asks for a brand-new session unconditionally.
            None if fresh_pending => client::AttachAction::New,
            // `mutx attach` with no id opens the TUI picker (ADR-0116).
            None if pick_pending => client::AttachAction::Picker,
            // Auto-bind a lone session (the daemon decides; several mean
            // the picker, which the Pick fallback below turns interactive).
            None => client::AttachAction::Attach(None),
        };
        fresh_pending = false;
        pick_pending = false;
        let handshake = client::connect(&info, action).await?;
        let (
            tx,
            rx,
            hosted_session_id,
            round_counter,
            transcript,
            round_interrupts,
            provider,
            model,
            command_catalog,
        ) = match handshake {
            client::Handshake::Attached {
                req_tx,
                resp_rx,
                session_id,
                round_counter,
                history,
                round_interrupts,
                provider,
                model,
                command_catalog,
            } => (
                req_tx,
                resp_rx,
                session_id,
                round_counter,
                history,
                round_interrupts,
                provider,
                model,
                command_catalog,
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
                        round_interrupts,
                        provider,
                        model,
                        command_catalog,
                    } => (
                        req_tx,
                        resp_rx,
                        session_id,
                        round_counter,
                        history,
                        round_interrupts,
                        provider,
                        model,
                        command_catalog,
                    ),
                    client::Handshake::Pick(_) => {
                        return Err("the daemon offered no session to pick from".into());
                    }
                }
            }
        };
        if yolo_pending {
            let _ = tx.send(muta_contracts::AgentRequest::SlashCommand(
                "/yolo on".to_string(),
            ));
            yolo_pending = false;
        }
        if let Some(prompt) = initial_prompt.take() {
            let _ = tx.send(muta_contracts::AgentRequest::Chat {
                text: prompt,
                images: Vec::new(),
                sent_at_ms: None,
            });
        }
        let mutx_config = mutx::config::TuiConfig::load();
        let input_history = mutx::config::load_history();
        let tui_config = mutx_config.clone();
        let input_history_config = mutx_config.input_history.clone();
        let startup_overlay = if dashboard_pending {
            dashboard_pending = false;
            mutx::StartupOverlay::Dashboard
        } else {
            mutx::StartupOverlay::None
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
            command_catalog,
            round_interrupts,
            tui_config,
            input_history_config,
            mutx::SessionSource::Remote {
                session_id: hosted_session_id,
            },
            None,
            startup_overlay,
        )
        .await?;
        save_history_bounded(outcome.history, mutx_config.input_history.dedup).await;
        match outcome.switch_to {
            Some(id) => {
                target = Some(id);
                continue;
            }
            None => return Ok(()),
        }
    }
}
