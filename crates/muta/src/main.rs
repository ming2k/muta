#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use muta_persistence::session;
use muta_runtime::client;
mod cli;
mod commands;
mod identity;
mod status;

/// This CLI's identity, handed to the engine as its opening system prompt.
/// Lives here (not in `muta-agent`) so the engine stays identity-agnostic
/// and a different frontend could reuse it as another agent.
use crate::identity::{DaemonUiBridge, master_code, muta_identity};
use cli::{CliArgs, DaemonAction, McpAction, Mode};

use std::path::PathBuf;
use std::sync::Arc;

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
    let parsed = match cli::parse(&std::env::args().skip(1).collect::<Vec<_>>()) {
        Ok(parsed) => parsed,
        Err(error) => {
            eprintln!("muta: {error}\n\nRun 'muta --help' for more information.");
            std::process::exit(2);
        }
    };

    let CliArgs {
        mode,
        project: project_override,
    } = parsed;

    match mode {
        Mode::Version => {
            println!("muta {}", env!("CARGO_PKG_VERSION"));
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
        Mode::Doctor => session::run_doctor(project_override.as_deref())
            .await
            .map_err(Into::into),
        Mode::Config(action) => commands::config::run(action),
        Mode::Auth(action) => commands::auth::run(action),
        Mode::Mcp(McpAction::Probe { name }) => commands::mcp::probe(&name).await,
        Mode::Mcp(action) => commands::mcp::run(action),
        Mode::Skill(action) => commands::skill::run(action).await,
        Mode::Session(action) => commands::session::run(action, project_override).await,
        Mode::Daemon(action) => run_daemon_action(action, project_override).await,
    }
}

/// `muta daemon start` (detached, the default): spawn the daemon in the
/// background and return. If a daemon is already running, report it
/// instead of spawning a second one.
fn detach_daemon(flags: &DaemonStart) -> Result<(), String> {
    if let Some(info) = client::discover(std::path::Path::new(".")) {
        return Err(format!(
            "a muta daemon is already running (pid {}, port {}). Stop it with `muta stop` before starting another.",
            info.pid, info.port
        ));
    }
    let program = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("muta"));
    let mut command = std::process::Command::new(&program);
    // The supervisor form: the child re-enters `start --fg` —
    // foreground by construction, its lifecycle flags from [daemon] config.
    // The child inherits `MUTA_HOME` in this process's environment (ADR-0121).
    command.args(["start", "--fg"]);
    // Every explicit start flag survives the detach: the child is the same
    // start the operator asked for, minus the daemonization. Dropping them
    // here would make `start --port N` silently bind the default —
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
    // Use the same process-level detachment as on-demand auto-spawn. Keeping
    // this in one helper prevents the two entry points from drifting into
    // subtly different session/process-group semantics.
    client::configure_daemon_detachment(&mut command);
    command
        .spawn()
        .map_err(|e| format!("could not spawn {}: {e}", program.display()))?;
    eprintln!(
        "muta: daemon started in the background (`muta status` to observe, `muta stop` to stop it)"
    );
    Ok(())
}

/// `muta daemon stop` (ADR-0100/0116): stop the running daemon through
/// the budget-aware shutdown pipeline (graceful control verb → SIGTERM →
/// SIGKILL). Stopping a daemon that is not running (or whose record is
/// stale) is a success — the operator's desired end state ("no daemon")
/// is already true.
async fn stop_daemon() -> Result<(), String> {
    let info = match client::discover(std::path::Path::new(".")) {
        Some(info) => info,
        None => {
            let lock_path = muta_runtime::serve_discovery::global_lock_path();
            if let Some(pid) = muta_persistence::lock::ProcessLock::probe_holder(&lock_path) {
                if client::is_process_alive(pid) {
                    client::DaemonInfo {
                        pid,
                        process_birth_token: muta_platform::process::process_identity(pid)
                            .ok()
                            .map(|identity| identity.birth_token),
                        port: muta_runtime::startup::env_default_port(),
                        token: None,
                        project_root: String::new(),
                        started_at: 0,
                        #[cfg(unix)]
                        uds_path: Some(muta_runtime::serve_discovery::default_uds_path()),
                        #[cfg(not(unix))]
                        uds_path: None,
                        local_endpoint: muta_runtime::serve_discovery::default_local_endpoint()
                            .ok(),
                        version: None,
                        grace_secs: None,
                        protocol: None,
                    }
                } else {
                    eprintln!("muta: no daemon is running.");
                    return Ok(());
                }
            } else {
                eprintln!("muta: no daemon is running.");
                return Ok(());
            }
        }
    };
    client::stop(&info).await?;
    eprintln!("muta: daemon stopped (pid {}).", info.pid);
    Ok(())
}
/// `muta daemon <action>` dispatch (ADR-0116: the daemon noun owns
/// start/stop/status/token).
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
        DaemonAction::Token => {
            let project_root = project_override
                .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
            let info = client::discover(&project_root).ok_or("no local muta daemon is running")?;
            match info.token {
                Some(token) => println!("{token}"),
                None => eprintln!("muta: daemon authentication is disabled."),
            }
            Ok(())
        }
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

/// `muta daemon start`: detached unless `--fg`. Detaching is the default
/// because the user asked for a *daemon*; `--fg` is the supervisor shape
/// (systemd/tmux foreground processes).
async fn run_daemon_foreground(flags: DaemonStart) -> Result<(), Box<dyn std::error::Error>> {
    let mut lifecycle = muta_runtime::host::LifecycleOptions::from_config();
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
    // honours MUTA_PORT so an isolated instance (ADR-0121) takes its own
    // port instead of contending with the host daemon on 9800.
    let port = flags
        .port
        .unwrap_or(muta_runtime::startup::env_default_port());
    let outcome = muta_runtime::host::run_with_gate(
        muta_runtime::host::HostIdentity {
            identity: muta_identity(),
            master: master_code(),
            ui: Arc::new(DaemonUiBridge),
        },
        muta_runtime::host::HostOptions {
            port,
            expose: if flags.public {
                muta_runtime::serve::ServeExpose::Public
            } else {
                muta_runtime::serve::ServeExpose::Local
            },
            token: None,
            // CLI flag wins over config; both default to the secure
            // posture (loopback token on, ADR-0105).
            local_auth: !flags.no_local_auth
                && muta_persistence::config::Config::load().daemon.local_auth,
            port_fallback: flags.port.is_none(),
            local_endpoint: Some(
                muta_runtime::serve_discovery::default_local_endpoint()
                    .map_err(std::io::Error::other)?,
            ),
        },
        std::sync::Arc::new(muta_runtime::shutdown::ShutdownGate::new()),
        lifecycle,
    )
    .await;
    match &outcome {
        muta_runtime::host::RunOutcome::Stopped { reason } => {
            eprintln!("muta: daemon stopped ({reason}).");
        }
        muta_runtime::host::RunOutcome::ForcedExit { reason } => {
            eprintln!(
                "muta: daemon stopped ({reason}); grace budget expired, stragglers were \
                 aborted — see the log."
            );
        }
        muta_runtime::host::RunOutcome::StartupFailed(what) => {
            eprintln!("muta: {what}");
        }
    }
    std::process::exit(outcome.exit_code());
}
