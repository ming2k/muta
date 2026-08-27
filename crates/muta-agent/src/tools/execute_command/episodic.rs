use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncWriteExt;

use crate::tools::execute_command::pipes::{OutputCollector, spawn_stream_readers};

pub fn workspace_sandbox_shell(
    command: &str,
    workspace_root: &std::path::Path,
    additional_roots: &[std::path::PathBuf],
) -> Result<tokio::process::Command, String> {
    muta_platform::workspace_sandbox::shell_with_roots(
        command,
        workspace_root,
        additional_roots,
        muta_platform::workspace_sandbox::WorkspaceAccess::ReadWrite,
        muta_platform::workspace_sandbox::NetworkAccess::Disabled,
    )
}

pub async fn run_episodic_command(
    command: &str,
    timeout_duration: Duration,
    isolation: muta_contracts::ShellIsolation,
    env: Arc<dyn muta_contracts::ExecutionEnvironment>,
    stdin_policy: muta_contracts::StdinPolicy,
    on_stream: &mut (dyn FnMut(muta_contracts::ToolStream) + Send + '_),
) -> Result<muta_contracts::ToolOutput, String> {
    // Resolve the stdin policy into the `Stdio` the child is spawned with.
    let stdin_bytes = match &stdin_policy {
        muta_contracts::StdinPolicy::Closed => None,
        muta_contracts::StdinPolicy::Prefilled { data } => Some(data.clone()),
    };
    let stdin_stdio = if stdin_bytes.is_some() {
        std::process::Stdio::piped()
    } else {
        std::process::Stdio::null()
    };

    let (mut child, process_tree) = {
        let mut invocation = match isolation {
            muta_contracts::ShellIsolation::Host => muta_platform::shell::native_shell(command),
            muta_contracts::ShellIsolation::Workspace => {
                let additional_roots = env.additional_roots();
                workspace_sandbox_shell(command, env.workspace_root(), &additional_roots)?
            }
        };
        invocation
            .kill_on_drop(true)
            .stdin(stdin_stdio)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        invocation.current_dir(env.workspace_root());
        muta_platform::process::spawn_owned(&mut invocation)
    }
    .map_err(|e| format!("Failed to execute and contain process tree: {e}"))?;

    // For a prefilled stdin, write the bytes into the pipe and drop our handle.
    if let Some(bytes) = stdin_bytes
        && let Some(mut child_stdin) = child.stdin.take()
    {
        let _ = child_stdin.write_all(bytes.as_bytes()).await;
        let _ = child_stdin.shutdown().await;
    }

    let stdout = child
        .stdout
        .take()
        .ok_or("failed to capture child stdout")?;
    let stderr = child
        .stderr
        .take()
        .ok_or("failed to capture child stderr")?;

    let mut readers = spawn_stream_readers(stdout, stderr);

    // L2 idle watchdog: a command that produces zero output for longer than
    // the idle budget is killed early to surface prompt blocking fast.
    //
    // The budget scales with the caller's `timeout` (one third, clamped to
    // [5s, 60s]) instead of a fixed 10s: a caller who budgets
    // more time for a legitimately quiet command (long sleeps, network waits,
    // `--quiet` builds) is not killed at an arbitrary short mark. The
    // idle watchdog is clamped to [5s, 60s].
    let idle_budget = idle_budget_for(timeout_duration);
    let timeout_deadline = tokio::time::Instant::now() + timeout_duration;

    let mut collector = OutputCollector::new();
    let mut idle_blocked = false;
    let mut timed_out = false;

    loop {
        let idle = tokio::time::sleep(idle_budget);
        let wall_timeout = tokio::time::sleep_until(timeout_deadline);
        tokio::pin!(idle);
        tokio::pin!(wall_timeout);

        tokio::select! {
            biased;
            _ = &mut wall_timeout => {
                timed_out = true;
                break;
            }
            _ = &mut idle => {
                idle_blocked = true;
                break;
            }
            msg = readers.rx.recv() => {
                match msg {
                    Some((stream, text)) => {
                        collector.push_line(stream, text, on_stream);
                    }
                    None => break, // channel closed -> normal completion
                }
            }
        }
    }

    if timed_out || idle_blocked {
        let _ = process_tree.terminate();
        readers.stdout_task.abort();
        readers.stderr_task.abort();
        collector.drain_remaining_rx(&mut readers.rx);
        let _ = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
    } else {
        let _ = readers.stdout_task.await;
        let _ = readers.stderr_task.await;
    }

    let exit = if timed_out || idle_blocked {
        None
    } else {
        child.wait().await.ok().and_then(|s| s.code())
    };

    let termination = if timed_out {
        muta_contracts::tool_output::ShellTermination::Timeout
    } else if idle_blocked {
        muta_contracts::tool_output::ShellTermination::IdleBlocked
    } else {
        muta_contracts::tool_output::ShellTermination::Exited
    };

    let (stdout, stderr, lines, truncated) = collector.apply_caps(exit);

    Ok(muta_contracts::ToolOutput::Shell {
        command: command.to_string(),
        stdout,
        stderr,
        lines,
        exit,
        truncated,
        termination,
    })
}

/// Idle-watchdog budget derived from the caller's wall-clock `timeout`.
///
/// One third of the timeout, clamped to [5s, 60s]: callers budgeting
/// more room for a legitimately quiet command (long sleeps, network waits,
/// `--quiet` builds) get proportionally more idle tolerance instead of
/// being killed at a fixed short mark.
pub fn idle_budget_for(timeout: Duration) -> Duration {
    let third = timeout / 3;
    third.clamp(Duration::from_secs(5), Duration::from_secs(60))
}
