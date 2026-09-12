#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use muta_client as client;
use mutx::start_tui;
mod cli;
mod headless;
use cli::{CliArgs, Mode};

use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    ensure_dev_environment();
    let _tracing_guard = muta_client::init_tracing();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let result = runtime.block_on(run());
    runtime.shutdown_background();
    result
}

fn ensure_dev_environment() {
    let is_debug_build = cfg!(debug_assertions);
    let dev_opt_out = std::env::var("MUTX_NO_DEV").map(|v| v != "0").unwrap_or(false);
    let dev_mode = (is_debug_build && !dev_opt_out)
        || std::env::var("MUTX_DEV").map(|v| v != "0").unwrap_or(false)
        || std::env::var("MUTA_DEV").map(|v| v != "0").unwrap_or(false)
        || std::env::var("MUTX_DEV_TOAST").is_ok()
        || std::env::var("MUTX_TOAST").is_ok();

    if !dev_mode {
        return;
    }

    // 1. Isolate home so it never conflicts with host installed muta
    if std::env::var_os("MUTA_HOME").is_none() {
        let dev_home = if let Ok(current) = std::env::current_exe() {
            if let Some(target) = current.parent().and_then(|p| p.parent()) {
                target.join("muta-dev")
            } else {
                std::env::temp_dir().join("muta-dev")
            }
        } else {
            std::env::temp_dir().join("muta-dev")
        };

        let _ = std::fs::create_dir_all(&dev_home);
        unsafe {
            std::env::set_var("MUTA_HOME", &dev_home);
        }
        let _ = muta_paths::paths::set_default(muta_paths::paths::Dirs::system());
    }

    // 2. Point MUTA_BIN to local source-built muta, building it if not yet present
    if std::env::var_os("MUTA_BIN").is_none() {
        if let Ok(current) = std::env::current_exe() {
            let sibling = current.with_file_name(format!("muta{}", std::env::consts::EXE_SUFFIX));
            if !sibling.is_file() {
                eprintln!("[mutx-dev] Local muta binary not found at {}. Compiling via cargo...", sibling.display());
                let status = std::process::Command::new("cargo")
                    .args(["build", "-p", "muta"])
                    .status();
                if let Ok(status) = status && !status.success() {
                    eprintln!("[mutx-dev] Warning: Failed to build local muta from source.");
                }
            }
            if sibling.is_file() {
                unsafe {
                    std::env::set_var("MUTA_BIN", &sibling);
                }
            }
        }
    }
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
        unattended: unattended_at_start,
        role,
        resume,
        no_confinement,
        interactive,
        prompt,
        json: _,
        remote,
        token,
        ..
    } = parsed;
    let confined_at_start = !no_confinement;

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
        Mode::Dashboard => {
            run_dashboard(project_override, unattended_at_start, confined_at_start).await
        }
        Mode::Settings { category } => {
            let cat_str = category.or_else(|| {
                std::env::var("MUTX_SETTINGS_NAV")
                    .or_else(|_| std::env::var("MUTX_SETTINGS_CATEGORY"))
                    .ok()
            });
            let cat = cat_str
                .as_deref()
                .filter(|s| !s.is_empty())
                .and_then(mutx::views::ConfigCategory::from_name)
                .map(|c| c as usize);
            run_attached(
                None,
                true,
                project_override,
                unattended_at_start,
                confined_at_start,
                role.clone(),
                resume,
                mutx::StartupOverlay::Settings { category: cat },
                prompt,
            )
            .await
        }
        Mode::Attach { id } => {
            let overlay =
                mutx::StartupOverlay::resolve_from_env().unwrap_or(mutx::StartupOverlay::None);
            run_attached(
                id,
                false,
                project_override,
                unattended_at_start,
                confined_at_start,
                role.clone(),
                resume,
                overlay,
                None,
            )
            .await
        }
        Mode::Run { prompt } => {
            if interactive {
                // `run -i` deliberately switches to the TUI with the prompt.
                let overlay =
                    mutx::StartupOverlay::resolve_from_env().unwrap_or(mutx::StartupOverlay::None);
                run_attached(
                    None,
                    true,
                    project_override,
                    unattended_at_start,
                    confined_at_start,
                    role.clone(),
                    resume,
                    overlay,
                    Some(prompt),
                )
                .await
            } else {
                headless::run_headless(
                    prompt,
                    parsed.json,
                    project_override,
                    unattended_at_start,
                    confined_at_start,
                    role.clone(),
                    resume,
                    remote,
                    token,
                )
                .await
            }
        }
        Mode::Fresh => {
            let overlay =
                mutx::StartupOverlay::resolve_from_env().unwrap_or(mutx::StartupOverlay::None);
            run_attached(
                None,
                true,
                project_override,
                unattended_at_start,
                confined_at_start,
                role.clone(),
                resume,
                overlay,
                prompt,
            )
            .await
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
    unattended_at_start: bool,
    confined_at_start: bool,
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
        unattended_at_start,
        confined_at_start,
        None,
        false,
        mutx::StartupOverlay::Dashboard,
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
    unattended_at_start: bool,
    confined_at_start: bool,
    role: Option<String>,
    resume: bool,
    initial_overlay: mutx::StartupOverlay,
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
    // The startup overlay (dashboard, settings, sessions picker) raises on
    // the first TUI entry only; a `/host` switch re-attaches into an ordinary
    // conversation view (the overlay does not re-arm).
    let mut startup_overlay_pending = initial_overlay;
    let init_options =
        muta_contracts::SessionInitOptions::new(unattended_at_start, confined_at_start)
            .with_role(role.clone())
            .with_resume(resume);
    // Re-attach loop: returning from the TUI with a `/host` switch target
    // re-connects to that session instead of exiting (ADR-0096).
    loop {
        let action = match &target {
            Some(id) => client::AttachAction::Attach(Some(id.clone())),
            // Bare `mutx` asks for a brand-new session unconditionally.
            None if fresh_pending => client::AttachAction::New(if init_options.is_default() {
                None
            } else {
                Some(init_options.clone())
            }),
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
            retry_resolutions,
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
                retry_resolutions,
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
                retry_resolutions,
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
                        retry_resolutions,
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
                        retry_resolutions,
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
        if let Some(prompt) = initial_prompt.take() {
            tx.send(muta_contracts::AgentRequest::Prompt {
                text: prompt,
                images: Vec::new(),
                sent_at_ms: None,
            })
            .map_err(|error| format!("daemon link lost before initial prompt: {error}"))?;
        }
        let mutx_config = mutx::config::TuiConfig::load();
        // History is hydrated from the daemon (SSOT) after the TUI starts;
        // the frontend never opens the shared database itself (ADR-0197).
        let input_history = Vec::new();
        let tui_config = mutx_config.clone();
        let input_history_config = mutx_config.input_history.clone();
        let startup_overlay =
            std::mem::replace(&mut startup_overlay_pending, mutx::StartupOverlay::None);
        let exit_tx = tx.clone();
        let outcome = start_tui(
            tx,
            rx,
            mutx::TuiLaunchConfig {
                initial_provider: provider,
                initial_model: model,
                input_history,
                initial_messages: transcript,
                initial_commands: Vec::new(),
                initial_round_count: round_counter,
                command_catalog,
                initial_round_interrupts: round_interrupts,
                initial_retry_resolutions: retry_resolutions,
                tui_config,
                input_history_config,
                session: mutx::SessionSource::Remote {
                    session_id: hosted_session_id,
                },
                token_ledger: None,
                startup_overlay,
            },
        )
        .await?;
        // Exit flush: merge the final buffer into the daemon's store. Each
        // prompt was already recorded during the session, so a lost flush at
        // most drops the last dedup pass — never the entries themselves.
        if let Err(error) = exit_tx.send(muta_contracts::AgentRequest::RecordInputHistory {
            entries: outcome.history,
            dedup: mutx_config.input_history.dedup,
        }) {
            tracing::error!(%error, "daemon link lost before exit history flush");
        }
        match outcome.switch_to {
            Some(id) => {
                target = Some(id);
                continue;
            }
            None => return Ok(()),
        }
    }
}
