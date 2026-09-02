#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use muta_runtime::client;
use mutx::start_tui;
mod cli;
mod headless;
use cli::{CliArgs, Mode};

use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _tracing_guard = muta_runtime::startup::init_tracing();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let result = runtime.block_on(run());
    runtime.shutdown_background();
    result
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
        delegated: delegated_at_start,
        interactive,
        prompt,
        json: _,
        remote,
        token,
        ..
    } = parsed;

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
        Mode::Dashboard => run_dashboard(project_override, delegated_at_start).await,
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
                delegated_at_start,
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
                delegated_at_start,
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
                    delegated_at_start,
                    overlay,
                    Some(prompt),
                )
                .await
            } else {
                headless::run_headless(
                    prompt,
                    parsed.json,
                    project_override,
                    delegated_at_start,
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
                delegated_at_start,
                overlay,
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
    delegated_at_start: bool,
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
        delegated_at_start,
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
    delegated_at_start: bool,
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
    let mut delegated_pending = delegated_at_start;
    // The startup overlay (dashboard, settings, sessions picker) raises on
    // the first TUI entry only; a `/host` switch re-attaches into an ordinary
    // conversation view (the overlay does not re-arm).
    let mut startup_overlay_pending = initial_overlay;
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
        if delegated_pending {
            let _ = tx.send(muta_contracts::AgentRequest::SlashCommand(
                "/delegate on".to_string(),
            ));
            delegated_pending = false;
        }
        if let Some(prompt) = initial_prompt.take() {
            let _ = tx.send(muta_contracts::AgentRequest::Prompt {
                text: prompt,
                images: Vec::new(),
                sent_at_ms: None,
            });
        }
        let mutx_config = mutx::config::TuiConfig::load();
        let input_history = mutx::config::load_history();
        let tui_config = mutx_config.clone();
        let input_history_config = mutx_config.input_history.clone();
        let startup_overlay =
            std::mem::replace(&mut startup_overlay_pending, mutx::StartupOverlay::None);
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
