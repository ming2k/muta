use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncWriteExt;

use crate::tools::execute_command::pipes::{
    OutputCollector, StreamReaders, spawn_stream_readers,
};
use tokio::task::JoinHandle;

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

/// The handle the caller must keep alive when a foreground child is handed to
/// the fabric (ADR-0190 detach-on-budget): stdout/stderr stay piped, and the
/// adopted owner (the job manager) drains them.
struct AdoptableChild {
    child: tokio::process::Child,
    process_tree: muta_platform::process::OwnedProcessTree,
}

impl muta_contracts::CrateChildBridge for AdoptableChild {
    fn try_wait(&mut self) -> Result<Option<i32>, String> {
        self.child
            .try_wait()
            .map(|s| s.and_then(|st| st.code()))
            .map_err(|e| e.to_string())
    }

    fn kill(&mut self) -> Result<(), String> {
        // Signal the whole process tree; the child itself is reaped by the
        // fabric's poll loop on its next try_wait.
        self.process_tree
            .terminate()
            .map_err(|e| format!("process tree termination failed: {e}"))
    }
}

/// Ownership shuttle for the detach-then-fallback flow: handles are moved out
/// for the adoption attempt and restored if the fabric refuses.
struct StreamReadersFallback {
    rx: Option<tokio::sync::mpsc::UnboundedReceiver<(muta_contracts::tool_output::ShellStream, String)>>,
    stdout_task: Option<JoinHandle<()>>,
    stderr_task: Option<JoinHandle<()>>,
    child: Option<tokio::process::Child>,
    process_tree: Option<muta_platform::process::OwnedProcessTree>,
}

impl StreamReadersFallback {
    fn take(
        readers: &mut StreamReaders,
        child: tokio::process::Child,
        process_tree: muta_platform::process::OwnedProcessTree,
    ) -> Self {
        Self {
            rx: Some(std::mem::replace(
                &mut readers.rx,
                tokio::sync::mpsc::unbounded_channel().1,
            )),
            stdout_task: Some(std::mem::replace(
                &mut readers.stdout_task,
                tokio::spawn(async {}),
            )),
            stderr_task: Some(std::mem::replace(
                &mut readers.stderr_task,
                tokio::spawn(async {}),
            )),
            child: Some(child),
            process_tree: Some(process_tree),
        }
    }

    /// Restore the drained handles after a refused adoption, returning the
    /// child and its process tree for the legacy kill path.
    fn restore(mut self) -> (StreamReaders, tokio::process::Child, muta_platform::process::OwnedProcessTree) {
        let readers = StreamReaders {
            rx: self.rx.take().unwrap(),
            stdout_task: self.stdout_task.take().unwrap(),
            stderr_task: self.stderr_task.take().unwrap(),
        };
        (readers, self.child.take().unwrap(), self.process_tree.take().unwrap())
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn run_episodic_command(
    command: &str,
    timeout_duration: Duration,
    isolation: muta_contracts::ShellIsolation,
    env: Arc<dyn muta_contracts::ExecutionEnvironment>,
    stdin_policy: muta_contracts::StdinPolicy,
    on_stream: &mut (dyn FnMut(muta_contracts::ToolStream) + Send + '_),
    job_service: Option<Arc<dyn muta_contracts::BackgroundJobService>>,
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

    let (mut child, mut process_tree) = {
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
    // [5s, 480s]): a caller who budgets more time for a legitimately quiet
    // command (long compiles, network waits, `--quiet` builds, or output
    // buffered by a pipe like `… | tail`) is not killed at an arbitrary
    // short mark. The default (1800s) tolerates 8 minutes of silence.
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

    // ADR-0190 detach-on-budget: when the idle budget fires but the process
    // produced output before going quiet (output-then-silence is the service
    // signature), the child is NOT killed — it is adopted by the background
    // job fabric, which keeps draining its pipes and notifies the session on
    // exit. Silence-from-birth (a stdin prompt the agent cannot answer)
    // still kills: there is nothing worth adopting.
    if idle_blocked && job_service.is_some() && !collector.is_empty() {
        {
            let mut readers_fallback = StreamReadersFallback::take(&mut readers, child, process_tree);
            let mut rx = readers_fallback.rx.take().unwrap();
            collector.drain_remaining_rx(&mut rx);
            drop(rx);
            if let Some(h) = readers_fallback.stdout_task.take() { h.abort(); }
            if let Some(h) = readers_fallback.stderr_task.take() { h.abort(); }

            let pid = readers_fallback.child.as_ref().and_then(|c| c.id()).unwrap_or_default();
            let adoptable = AdoptableChild {
                child: readers_fallback.child.take().unwrap(),
                process_tree: readers_fallback.process_tree.take().unwrap(),
            };
            let captured: Vec<String> = collector
                .lines()
                .iter()
                .map(|l| l.text.clone())
                .collect();

            match job_service
                .as_ref()
                .unwrap()
                .adopt_process(
                    command.to_string(),
                    None,
                    muta_contracts::AdoptionInfo {
                        captured_lines: captured,
                        pid,
                        child: Box::new(adoptable),
                    },
                )
                .await
            {
                Ok(info) => {
                    collector.flush_stream(on_stream);
                    let (stdout, stderr, lines, truncated) = collector.apply_caps(None);
                    return Ok(muta_contracts::ToolOutput::Shell {
                        command: command.to_string(),
                        stdout,
                        stderr,
                        lines,
                        exit: None,
                        truncated,
                        termination: muta_contracts::tool_output::ShellTermination::Detached,
                        detached_job_id: Some(info.id.0),
                    });
                }
                Err(_) => {
                    // Adoption refused: restore the handles and fall through
                    // to the legacy kill path below.
                    let (r, c, t) = readers_fallback.restore();
                    readers = r;
                    child = c;
                    process_tree = t;
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
    collector.flush_stream(on_stream);

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
        detached_job_id: None,
    })
}

/// Idle-watchdog budget derived from the caller's wall-clock `timeout`.
///
/// One third of the timeout, clamped to [5s, 480s]: callers budgeting
/// more room for a legitimately quiet command (long compiles, network
/// waits, `--quiet` builds) get proportionally more idle tolerance, and
/// even the default (1800s) tolerates 8 minutes of silence — which
/// matters because output buffered by a pipe (`… | tail`) is
/// indistinguishable from silence until the pipe closes.
pub fn idle_budget_for(timeout: Duration) -> Duration {
    let third = timeout / 3;
    third.clamp(Duration::from_secs(5), Duration::from_secs(480))
}
