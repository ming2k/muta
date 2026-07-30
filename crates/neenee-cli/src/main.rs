#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use crate::tui::start_tui;
use neenee_persistence::config::Config;
use neenee_persistence::session;
mod identity;
mod remote;
#[cfg(debug_assertions)]
mod showcase;
mod tui;

pub(crate) use neenee_transport::startup;
use neenee_transport::session_view::short_session_id;

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

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _tracing_guard = init_tracing();

    // Parse CLI up front. `showcase` (debug-only) and `doctor` are purely
    // local: no agent, no session, no network. They must short-circuit BEFORE
    // the session harness is assembled — otherwise they would pay the full
    // production startup cost (skill scan, MCP connects,
    // agent construction) for nothing. The Showcase variant only exists under
    // `debug_assertions`, so the guard here mirrors it.
    let (startup, project_override, autopilot_at_start, single_instance) =
        parse_args(std::env::args().skip(1).collect());

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
            project_override,
            autopilot_at_start,
        )
        .await;
    }

    // Assemble the session harness (ADR-0037 Step 6): channels, config,
    // stores, provider/skills/toolset wiring, agent, MCP, restores, and the
    // `SessionDriver` — shared with every frontend binary. This binary
    // supplies its identity, coding principal, and clipboard bridge.
    // `neenee resume` (no id) opens the sessions picker at startup *instead
    // of* loading any session, so closing it must quit rather than drop into
    // an empty chat. Captured here because `startup` moves into `assemble`.
    let startup_picker = matches!(startup, StartupMode::Picker);
    let boot = bootstrap::assemble(BootstrapParams {
        identity: neenee_identity(),
        principal: principal_code(),
        ui: Arc::new(crate::tui::clipboard::TuiClipboard),
        startup,
        project_root: project_override,
        autopilot: autopilot_at_start,
        single_instance,
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
        process_lock,
    } = boot;
    // The advisory process lock (ADR-0018, `--single-instance`) releases on
    // drop — hold the guard in `main`'s scope for the process lifetime.
    let _process_lock = process_lock;
    let initial_round_count = session.round_counter().await;
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
        initial_round_count,
        custom_command_suggestions,
        tui_config,
        crate::tui::SessionSource::Local(session),
        Some(token_ledger),
        startup_picker,
    )
    .await
    {
        Ok(history) => {
            // SessionEnd hooks (ADR-0025): observers fire on clean exit.
            agent_for_session_end.fire_session_end().await;
            let _ = Config::save_history(&history);
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

/// Attach-mode entry (`neenee --attach [id]`): find or spawn the project's
/// session server, connect over WebSocket, and drive the hosted session with
/// the ordinary TUI. This process is only a client — the server owns the
/// session lifecycle (and fires SessionEnd hooks on its own shutdown), so
/// none of that runs here.
async fn run_attached(
    session_id: Option<String>,
    project_override: Option<PathBuf>,
    autopilot_at_start: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let project_root = project_override.unwrap_or_else(|| {
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
    });
    let info = remote::ensure_server(
        &project_root,
        session_id.as_deref(),
        autopilot_at_start,
    )
    .await?;
    let (tx, rx, hosted_session_id, round_counter, transcript) = remote::connect(&info).await?;

    // If the client requested autopilot, make sure the hosted session is in
    // autopilot regardless of who created it:
    //   - when the client spawned the server it already passed `--autopilot`,
    //     so this is a harmless idempotent re-affirmation;
    //   - when an unrelated server was already running, the spawn path never
    //     ran, so this is the only way to apply the client's intent.
    // `/autopilot on` flows over the same wire path the TUI uses, so the
    // server mutates state and emits `AutopilotChanged` — the status bar
    // reflects the flip exactly like a hand-typed `/autopilot on`.
    if autopilot_at_start {
        let _ = tx.send(neenee_core::AgentRequest::SlashCommand(
            "/autopilot on".to_string(),
        ));
    }
    // Input history and `[tui]` presentation prefs are client-side concerns —
    // the server knows nothing of them — so they load from the LOCAL config
    // exactly as the standalone path does.
    let input_history = Config::load_history();
    let tui_config = Config::load().tui;
    let history = start_tui(
        tx,
        rx,
        // Provider/model placeholders. The hint bar renders only the model
        // label and skips an empty one cleanly (no context meter either); the
        // picker falls back to the server snapshot's default id when the
        // placeholder matches no row, and the first picker snapshot /
        // `ProviderSwitched` from the server repopulates the real names.
        "attached".to_string(),
        String::new(),
        input_history,
        transcript,
        round_counter,
        // Server-side custom commands are unknown to the client in v1 — the
        // handshake carries no command list — so there are no suggestions.
        vec![],
        tui_config,
        crate::tui::SessionSource::Remote {
            session_id: hosted_session_id,
        },
        // Token accounting lives server-side; the client installs no ledger
        // (the TokenReport modal surfaces a notice instead of an empty report).
        None,
        // Attach mode connects to a hosted session; the sessions picker is
        // never the entry, so closing a `/sessions` modal just dismisses it.
        false,
    )
    .await?;
    // No SessionEnd hooks: the client never owned the session. The composer
    // history is this client's own — persist it like the standalone path.
    let _ = Config::save_history(&history);
    Ok(())
}

#[cfg(test)]
mod tests;
