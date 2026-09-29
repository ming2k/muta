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

/// A running child whose output streams are being drained and whose input
/// channels may remain writable. Owns the process tree so every exit path
/// (answered, unanswered, timeout, cancel) reaps the whole group.
struct RunningChild {
    child: tokio::process::Child,
    tree: muta_platform::process::OwnedProcessTree,
    /// Child terminal master (supervised only). Because the child's stdin *is*
    /// the terminal slave, an answer to either a stdin read or a `/dev/tty`
    /// read is a write here.
    tty: Option<muta_platform::process::PtyMaster>,
}

impl RunningChild {
    /// Write one line of input into the child's terminal.
    fn answer(&self, data: &str) -> std::io::Result<()> {
        self.tty
            .as_ref()
            .ok_or_else(|| std::io::Error::other("no terminal to write the answer to"))?
            .write_input(data)
    }
}

/// How the drain loop ended.
enum Supervision {
    /// The child exited on its own.
    Exited(Option<i32>),
    /// An input wait was detected and no answer was supplied (no handler, or
    /// the operator declined). The child was killed.
    InputUnanswered,
    /// Wall-clock ceiling reached while still running.
    TimedOut,
    /// No output for the idle budget with no detectable input wait — the
    /// ambiguous quiet case (a compiling build, or a pipe-buffered command).
    IdleBlocked,
    /// Continuous streaming flood (ADR-0257).
    StreamGuarded,
}

/// Policy knobs for one command run: the wall-clock budget, raw-output mode,
/// and the runtime input supervisor. Bundled so the runner's signature does
/// not grow per feature.
pub struct RunPolicy<'a> {
    pub timeout: Duration,
    pub raw: bool,
    pub handler: Option<&'a dyn muta_contracts::InputHandler>,
}

pub async fn run_episodic_command(
    command: &str,
    isolation: muta_contracts::ShellIsolation,
    env: Arc<dyn muta_contracts::ExecutionEnvironment>,
    input: muta_contracts::InputContract,
    policy: RunPolicy<'_>,
    on_stream: &mut (dyn FnMut(muta_contracts::ToolStream) + Send + '_),
) -> Result<muta_contracts::ToolOutput, String> {
    let mut invocation = match isolation {
        muta_contracts::ShellIsolation::Host => muta_platform::shell::native_shell(command),
        muta_contracts::ShellIsolation::Workspace => {
            let additional_roots = env.additional_roots();
            workspace_sandbox_shell(command, env.workspace_root(), &additional_roots)?
        }
    };
    invocation.current_dir(env.workspace_root());
    muta_platform::shell::configure_headless_env(&mut invocation);
    invocation.kill_on_drop(true);

    let (running, expectation) = match &input {
        muta_contracts::InputContract::Sealed => {
            invocation.stdin(std::process::Stdio::null());
            (spawn_plain(&mut invocation)?, None)
        }
        muta_contracts::InputContract::Prefilled { data } => {
            invocation.stdin(std::process::Stdio::piped());
            let mut running = spawn_plain(&mut invocation)?;
            if let Some(mut stdin) = running.child.stdin.take() {
                let _ = stdin.write_all(data.as_bytes()).await;
                let _ = stdin.shutdown().await;
            }
            (running, None)
        }
        muta_contracts::InputContract::Supervised { expectation } => {
            let spawned = muta_platform::process::spawn_supervised(&mut invocation)
                .map_err(|e| format!("Failed to execute and contain supervised process tree: {e}"))?;
            let running = RunningChild {
                child: spawned.child,
                tree: spawned.tree,
                tty: spawned.tty,
            };
            (running, expectation.clone())
        }
    };

    run_loop(command, running, policy, expectation, on_stream).await
}

/// Spawn an owned child with piped stdout/stderr (stdin already configured by
/// the caller). Only used for the sealed/prefilled contracts.
fn spawn_plain(invocation: &mut tokio::process::Command) -> Result<RunningChild, String> {
    invocation
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let (child, tree) = muta_platform::process::spawn_owned(invocation)
        .map_err(|e| format!("Failed to execute and contain process tree: {e}"))?;
    Ok(RunningChild {
        child,
        tree,
        tty: None,
    })
}

/// The single drain loop for every input contract. It always runs the semantic
/// examiner: on detecting a genuine input wait it injects an answer when a
/// handler is present (supervised), and fast-fails otherwise (sealed — the
/// unattended path). Detection is kernel-evidence based (see
/// [`muta_platform::process::WaitState`]), so legitimate quiet computation is
/// never mistaken for a prompt.
async fn run_loop(
    command: &str,
    mut running: RunningChild,
    policy: RunPolicy<'_>,
    expectation: Option<muta_contracts::InputExpectation>,
    on_stream: &mut (dyn FnMut(muta_contracts::ToolStream) + Send + '_),
) -> Result<muta_contracts::ToolOutput, String> {
    let RunPolicy {
        timeout: timeout_duration,
        raw,
        handler,
    } = policy;
    let stdout = running
        .child
        .stdout
        .take()
        .ok_or("failed to capture child stdout")?;
    let stderr = running
        .child
        .stderr
        .take()
        .ok_or("failed to capture child stderr")?;
    let mut readers = spawn_stream_readers(stdout, stderr);

    let idle_budget = idle_budget_for(timeout_duration);
    let timeout_deadline = tokio::time::Instant::now() + timeout_duration;
    let examiner_floor = Duration::from_secs(5);

    let mut collector = OutputCollector::new();
    let mut last_output_at = tokio::time::Instant::now();
    let mut ticker = tokio::time::interval(Duration::from_millis(250));
    // Consecutive AwaitingInput samples with no CPU progress (stability gate),
    // and the previous sample for the progress comparison. Both are touched
    // only on the examiner path, never per output line.
    let mut stable_hits: u32 = 0;
    let mut prev_sample: Option<muta_platform::process::ProcessActivitySample> = None;

    let outcome = loop {
        tokio::select! {
            biased;
            msg = readers.rx.recv() => {
                match msg {
                    Some((stream, text)) => {
                        collector.push_line(stream, text, on_stream);
                        // The arrival of a line is itself the proof of progress;
                        // no kernel sample is needed. Sampling `/proc` here (once
                        // per line) would cost O(host processes) per line — a
                        // command emitting thousands of lines would be slowed to
                        // a crawl.
                        last_output_at = tokio::time::Instant::now();
                        if collector.is_stream_flooded(raw) {
                            break Supervision::StreamGuarded;
                        }
                    }
                    None => break Supervision::Exited(exit_code(&mut running.child).await),
                }
            }
            _ = ticker.tick() => {
                let now = tokio::time::Instant::now();
                if now >= timeout_deadline {
                    break Supervision::TimedOut;
                }
                let quiet_for = now.duration_since(last_output_at);
                // Only examine once the command has been quiet past the floor;
                // the ticker is coarse enough that this is a handful of samples
                // per second at worst, and the expensive `/proc` scan happens
                // only here — never on the per-line fast path.
                if quiet_for >= examiner_floor {
                    let sample = running.tree.sample_activity();
                    match sample.wait_state {
                        muta_platform::process::WaitState::AwaitingInput { channel } => {
                            // Stability: require two consecutive samples with no
                            // CPU progress, so a momentary block does not trip.
                            let progressed = prev_sample
                                .is_some_and(|prev| sample.total_cpu_ticks > prev.total_cpu_ticks);
                            stable_hits = if progressed { 0 } else { stable_hits + 1 };
                            prev_sample = Some(sample);
                            if stable_hits >= 2 {
                                stable_hits = 0;
                                match handler {
                                    Some(handler) => {
                                        match await_answer(
                                            command,
                                            channel,
                                            expectation.as_ref(),
                                            handler,
                                        )
                                        .await
                                        {
                                            Some(data) => {
                                                if let Err(error) = running.answer(&data) {
                                                    tracing::warn!(%error, "failed to write operator input");
                                                }
                                                prev_sample = None;
                                                last_output_at = tokio::time::Instant::now();
                                            }
                                            None => break Supervision::InputUnanswered,
                                        }
                                    }
                                    // No supervisor: fast-fail on the detected
                                    // wait (the unattended contract).
                                    None => break Supervision::InputUnanswered,
                                }
                            }
                        }
                        muta_platform::process::WaitState::Running => {
                            prev_sample = Some(sample);
                            stable_hits = 0;
                        }
                        _ => {
                            stable_hits = 0;
                        }
                    }
                }
                if quiet_for >= idle_budget {
                    break Supervision::IdleBlocked;
                }
            }
        }
    };

    let termination = match outcome {
        Supervision::Exited(exit) => {
            let _ = readers.stdout_task.await;
            let _ = readers.stderr_task.await;
            collector.flush_stream(on_stream);
            return finish_output(
                command,
                collector,
                exit,
                muta_contracts::tool_output::ShellTermination::Exited,
                raw,
            );
        }
        Supervision::InputUnanswered => {
            muta_contracts::tool_output::ShellTermination::InputUnanswered
        }
        Supervision::TimedOut => muta_contracts::tool_output::ShellTermination::Timeout,
        Supervision::IdleBlocked => muta_contracts::tool_output::ShellTermination::IdleBlocked,
        Supervision::StreamGuarded => muta_contracts::tool_output::ShellTermination::StreamGuard,
    };
    let _ = running.tree.terminate();
    readers.stdout_task.abort();
    readers.stderr_task.abort();
    collector.drain_remaining_rx(&mut readers.rx);
    let _ = tokio::time::timeout(Duration::from_secs(5), running.child.wait()).await;
    collector.flush_stream(on_stream);
    finish_output(command, collector, None, termination, raw)
}

async fn exit_code(child: &mut tokio::process::Child) -> Option<i32> {
    child.wait().await.ok().and_then(|s| s.code())
}

/// Park the runtime input wait for the operator and return the answer, or
/// `None` when no answer is supplied (cancelled, or no reachable human
/// channel). The prompt names the channel the answer is written into.
async fn await_answer(
    command: &str,
    channel: muta_platform::process::InputChannel,
    expectation: Option<&muta_contracts::InputExpectation>,
    handler: &dyn muta_contracts::InputHandler,
) -> Option<String> {
    let channel = match channel {
        muta_platform::process::InputChannel::Pipe => muta_contracts::InputChannel::Stdin,
        muta_platform::process::InputChannel::Terminal => {
            muta_contracts::InputChannel::ControllingTty
        }
    };
    let prompt = muta_contracts::InputPrompt {
        command: command.to_string(),
        prompt: expectation
            .map(|e| e.prompt.clone())
            .unwrap_or_else(|| format!("This command is waiting for input ({command}):")),
        secret: expectation.map(|e| e.secret).unwrap_or(false),
        channel,
    };
    handler.resolve(prompt).await
}

fn finish_output(
    command: &str,
    collector: OutputCollector,
    exit: Option<i32>,
    termination: muta_contracts::tool_output::ShellTermination,
    raw: bool,
) -> Result<muta_contracts::ToolOutput, String> {
    let (stdout, stderr, lines, truncated) = collector.apply_caps_ex(exit, raw);
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

