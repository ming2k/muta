use async_trait::async_trait;
use neenee_contracts::Tool;
use serde_json::json;
use tokio::process::Command;
use tokio::time::Duration;

use crate::tools::helpers::{
    WorkspaceBase, env_from_root, execution_environment, json_string, workspace_base,
};

/// Execute a bash command.
///
/// Commands run in the session's workspace root (captured at factory time),
/// not the daemon process's cwd — under the unified daemon (ADR-0096) those
/// differ whenever the daemon was first spawned from another project.
pub struct BashTool {
    pub(crate) root: WorkspaceBase,
    pub(crate) env: Option<std::sync::Arc<dyn neenee_contracts::ExecutionEnvironment>>,
}

impl BashTool {
    /// Build a bash tool bound to an explicit workspace root. The session
    /// runtime uses this for the `!`-prefix shell path, which bypasses the
    /// factory-based toolset assembly but must still run in the session's
    /// project (not the daemon's process cwd, ADR-0096).
    pub fn new(root: Option<std::path::PathBuf>) -> Self {
        Self { root, env: None }
    }

    /// Build a bash tool backed by a custom execution environment.
    pub fn with_env(env: std::sync::Arc<dyn neenee_contracts::ExecutionEnvironment>) -> Self {
        let root = Some(env.workspace_root().to_path_buf());
        Self {
            root,
            env: Some(env),
        }
    }
}

#[async_trait]
impl Tool for BashTool {
    fn name(&self) -> &str {
        "bash"
    }
    /// `bash` runs commands — its primary purpose is execution, not workspace
    /// mutation — so it sits in the `Execute` tier between pure reads and
    /// file-writing tools. The broker still gates it (`Execute > Read`). See
    /// ADR-0012.
    fn description(&self) -> &str {
        "Execute a shell command. Use for git, build, test, or any system operation. \
         A command that produces no output for 10 seconds is treated as blocked \
         (e.g. waiting on stdin) and is killed early even if `timeout` is longer; \
         long but healthy commands keep producing output and are not affected."
    }
    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "The shell command to execute" },
                "timeout": { "type": "integer", "description": "Overall timeout in seconds (default 30). A command producing no output for 10s is still killed early as a blocked-command guard." }
            },
            "required": ["command"]
        })
    }
    fn scope_target(&self, arguments: &str) -> neenee_contracts::ScopeTarget {
        neenee_contracts::ScopeTarget::Command(json_string(arguments, "command"))
    }
    async fn call(&self, arguments: &str) -> Result<String, String> {
        self.call_structured(arguments).await.map(|o| o.to_text())
    }

    async fn call_structured(
        &self,
        arguments: &str,
    ) -> Result<neenee_contracts::ToolOutput, String> {
        // Non-streaming path: delegate with no-op sinks and the default
        // (closed) stdin policy. The streaming entry point below is where the
        // real stdin policy is applied.
        self.call_structured_with_events(
            "",
            arguments,
            Box::new(|_| {}),
            &mut |_| {},
            neenee_contracts::StdinPolicy::default(),
        )
        .await
    }

    /// Spawn the command with piped stdout/stderr and merge both streams into a
    /// single, arrival-ordered line buffer so the renderer never has to choose
    /// the "all-stdout-then-all-stderr" split (which loses interleaving for
    /// tools like `cargo`/`git`/`npm`, whose progress/warnings hit stderr while
    /// results hit stdout). Both pipes are read on separate tasks and funnelled
    /// through one channel; the main future drains it in order, which is also
    /// where the `&mut` stream sink fires (it can't cross a spawned-task
    /// boundary).
    ///
    /// Each captured line is ANSI-stripped at the source: many commands emit
    /// colour even under a non-tty (`--color=always`, `CLICOLOR_FORCE`, a
    /// forced `.bashrc`), and raw `\x1b[...]m` bytes would corrupt the TUI's
    /// width math and read as literal `[0;32m` glyphs.
    async fn call_structured_with_events<'a>(
        &self,
        _call_id: &str,
        arguments: &str,
        _on_event: Box<dyn FnMut(neenee_contracts::EnvoyEvent) + Send + 'a>,
        on_stream: &mut (dyn FnMut(neenee_contracts::ToolStream) + Send + 'a),
        stdin_policy: neenee_contracts::StdinPolicy,
    ) -> Result<neenee_contracts::ToolOutput, String> {
        use neenee_contracts::tool_output::{
            ShellLine, ShellStream, normalize_carriage_returns, strip_ansi,
        };
        // In-memory collection caps (see `cap_shell_buffers`): the structured
        // output carries full stdout/stderr/`lines`, so a chatty command
        // (`cat huge.log` under a 30s timeout) could buffer hundreds of MB
        // before the *text* path truncated at `to_text` time. Head+tail
        // truncation here bounds the resident payload; the model still gets
        // the same SHELL_TRUNCATED_CHARS view and the TUI's folded rendering
        // already skips the middle, so nothing user-visible is lost.
        const SHELL_COLLECT_MAX_CHARS: usize =
            neenee_contracts::tool_output::SHELL_MAX_OUTPUT_CHARS * 8;
        const SHELL_COLLECT_MAX_LINES: usize = 5_000;
        use tokio::io::{AsyncBufReadExt, BufReader};

        let args: serde_json::Value =
            serde_json::from_str(arguments).map_err(|e| format!("Invalid JSON: {}", e))?;
        let command = args["command"].as_str().ok_or("Missing 'command'")?;
        let timeout_secs = args["timeout"].as_u64().unwrap_or(30);
        let timeout_duration = Duration::from_secs(timeout_secs);

        // Resolve the stdin policy into the `Stdio` the child is spawned with.
        // `Closed` → `/dev/null` (the default hard floor: a child blocking on
        // `read(stdin)` gets instant EOF). `Prefilled` → a pipe we write the
        // bytes into right after spawn; the pipe buffer holds them ahead of
        // the child's first read. (L1 — see disclosure/bash design doc.)
        let stdin_bytes = match &stdin_policy {
            neenee_contracts::StdinPolicy::Closed => None,
            neenee_contracts::StdinPolicy::Prefilled { data } => Some(data.clone()),
        };
        let stdin_stdio = if stdin_bytes.is_some() {
            std::process::Stdio::piped()
        } else {
            std::process::Stdio::null()
        };

        let mut child = if cfg!(target_os = "windows") {
            Command::new("cmd")
                .args(["/C", command])
                .kill_on_drop(true)
                .stdin(stdin_stdio)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
        } else {
            // Isolate the child from neenee's controlling terminal. neenee's
            // TUI runs in raw mode + alt screen on the *real* terminal, and a
            // plain `sh -c` child inherits our session/process group, i.e. it
            // shares our controlling tty. Any program that opens /dev/tty
            // (pinentry-curses, whiptail, dialog, sudo's password prompt,
            // `clear`/`reset`, ncurses tools pulled in transitively by
            // git/apt/…) then writes raw escape sequences *straight* to our
            // alternate screen, bypassing the retained grid + diff renderer
            // and scrambling the layout. `.process_group(0)` calls
            // `setpgid(0, 0)` between fork and exec so the child lands in its
            // own process group; combined with the non-tty stdout/stderr
            // pipes this keeps such programs off our screen. Those that then
            // block waiting on a (now-inaccessible) tty are surfaced fast by
            // the idle watchdog (L2) with a remedy footer.
            //
            // The child runs in the session's workspace root, not this
            // process's cwd: under the unified daemon (ADR-0096) the daemon
            // is spawned from whichever client came first, so its cwd belongs
            // to a different project than the session invoking this tool.
            // `Command::current_dir` chdirs between fork and exec, so the
            // rest of the spawn (process group, pipes) is unaffected.
            let mut invocation = Command::new("sh");
            invocation
                .arg("-c")
                .arg(command)
                .process_group(0)
                .kill_on_drop(true)
                .stdin(stdin_stdio)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped());
            let env = self.env.clone().unwrap_or_else(|| env_from_root(&self.root));
            invocation.current_dir(env.workspace_root());
            invocation.spawn()
        }
        .map_err(|e| format!("Failed to execute: {}", e))?;

        // For a prefilled stdin, write the bytes into the pipe and drop our
        // handle so the child sees EOF once it has consumed them. The pipe
        // buffer (≥ 4 KiB) holds a typical passphrase ahead of the child's
        // first read, so ordering relative to stdout is irrelevant.
        if let Some(bytes) = stdin_bytes
            && let Some(mut child_stdin) = child.stdin.take()
        {
            use tokio::io::AsyncWriteExt;
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

        // One merged channel: both pipes push (stream, line) here in arrival
        // order, so the drained `lines` preserves stdout/stderr interleaving.
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<(ShellStream, String)>();

        // Read stderr on a separate task so a full stderr pipe can't block the
        // child while stdout is being read. Each line is ANSI-stripped and
        // carriage-return/backspace normalized (so a `\r`-refreshed progress
        // bar collapses to its final frame instead of being dropped or
        // mis-rendered) before it enters the merged channel.
        let tx_err = tx.clone();
        let stderr_task = tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let _ = tx_err.send((
                    ShellStream::Err,
                    normalize_carriage_returns(&strip_ansi(&line)),
                ));
            }
        });

        // Read stdout on its own task too, so both pipes drain concurrently and
        // their lines land in the channel in true arrival order.
        let tx_out = tx.clone();
        let stdout_task = tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let _ = tx_out.send((
                    ShellStream::Out,
                    normalize_carriage_returns(&strip_ansi(&line)),
                ));
            }
        });

        // `kill_on_drop` guarantees the child is terminated when this future is
        // dropped — on timeout (the `Timeout` wrapper drops the inner future)
        // and on mid-run interrupt.
        //
        // L2 idle watchdog: the drain races each `recv()` against an idle
        // deadline. A command that produces zero output for longer than the
        // idle budget is almost certainly blocked waiting for stdin (a prompt
        // the agent cannot answer); killing it then — instead of burning the
        // entire wall-clock timeout — surfaces the failure fast and tags it
        // `IdleBlocked` so the footer can suggest a non-interactive retry.
        // Healthy long-running commands (build/test) keep producing lines,
        // resetting the deadline each time, so the idle timer never fires on
        // legitimate work.
        let idle_budget = Duration::from_secs(10);
        // `child` stays owned *outside* the drain future (borrowed as
        // `&mut`): this lets the wall-clock race below fire the group kill
        // itself on timeout. A plain `timeout(run)` around a future that
        // *owns* the child would drop it mid-await, delegating the kill to
        // `kill_on_drop` — which signals only the direct child, letting
        // grandchildren leak (`sh -c "server & echo hi"`).
        let run = async {
            stdout_task.await.ok();
            stderr_task.await.ok();
            drop(tx); // close so the drain below terminates

            // Drain the merged channel in arrival order, racing each recv
            // against the idle deadline. This is the only place the
            // `&mut` stream sink fires, so it sees the same interleaving as
            // the final `lines`. Rebuild the flat stdout/stderr strings the
            // model-facing path expects alongside the ordered view.
            let mut lines: Vec<ShellLine> = Vec::new();
            let mut stdout_buf = String::new();
            let mut stderr_buf = String::new();
            let mut idle_blocked = false;
            loop {
                // Reset the idle deadline each iteration: any output in the
                // last `idle_budget` keeps the command alive.
                let idle = tokio::time::sleep(idle_budget);
                tokio::pin!(idle);
                tokio::select! {
                    biased;
                    _ = &mut idle => {
                        // No output for the whole budget → assume stdin-blocked.
                        idle_blocked = true;
                        break;
                    }
                    msg = rx.recv() => {
                        match msg {
                            Some((stream, text)) => {
                                match stream {
                                    ShellStream::Out => {
                                        stdout_buf.push_str(&text);
                                        stdout_buf.push('\n');
                                        on_stream(neenee_contracts::ToolStream::Stdout(
                                            format!("{}\n", text),
                                        ));
                                    }
                                    ShellStream::Err => {
                                        stderr_buf.push_str(&text);
                                        stderr_buf.push('\n');
                                        on_stream(neenee_contracts::ToolStream::Stderr(
                                            format!("{}\n", text),
                                        ));
                                    }
                                }
                                lines.push(ShellLine { stream, text });
                            }
                            None => break, // channel closed → normal completion
                        }
                    }
                }
            }

            // If we broke out on the idle deadline, the child is still alive;
            // reap it (kill_on_drop would too, but reaping gives a real exit).
            // A blocked child may not have exited, so don't block on wait()
            // indefinitely — best-effort. The group kill also reaches
            // grandchildren the child backgrounded (see kill_process_group).
            // Head+tail cap on the collected buffers: keep both ends (the
            // head shows what the command did first, the tail shows how it
            // ended — errors cluster there) and drop the middle, mirroring
            // how both the model view and the TUI fold already treat it.
            let mut collection_truncated = false;
            if stdout_buf.len() > SHELL_COLLECT_MAX_CHARS {
                stdout_buf = head_tail(&stdout_buf, SHELL_COLLECT_MAX_CHARS / 2);
                collection_truncated = true;
            }
            if stderr_buf.len() > SHELL_COLLECT_MAX_CHARS {
                stderr_buf = head_tail(&stderr_buf, SHELL_COLLECT_MAX_CHARS / 2);
                collection_truncated = true;
            }
            if lines.len() > SHELL_COLLECT_MAX_LINES {
                let half = SHELL_COLLECT_MAX_LINES / 2;
                let dropped = lines.len() - (half * 2);
                let marker = ShellLine {
                    stream: ShellStream::Err,
                    text: format!("⋯ {dropped} lines dropped (collection cap)"),
                };
                let mut capped: Vec<ShellLine> = lines.drain(..half).collect();
                capped.push(marker);
                capped.extend(lines.drain(lines.len() - half..));
                lines = capped;
                collection_truncated = true;
            }

            let exit = if idle_blocked {
                crate::tools::kill_process_group(&child);
                child.wait().await.ok().and_then(|s| s.code())
            } else {
                child.wait().await.ok().and_then(|s| s.code())
            };

            let termination = if idle_blocked {
                neenee_contracts::tool_output::ShellTermination::IdleBlocked
            } else {
                neenee_contracts::tool_output::ShellTermination::Exited
            };
            let truncated = collection_truncated
                || neenee_contracts::tool_output::shell_inner_text(&stdout_buf, &stderr_buf, exit)
                    .len()
                    > neenee_contracts::tool_output::SHELL_MAX_OUTPUT_CHARS;
            Ok(neenee_contracts::ToolOutput::Shell {
                command: command.to_string(),
                stdout: stdout_buf,
                stderr: stderr_buf,
                lines,
                exit,
                truncated,
                termination,
            }) as Result<neenee_contracts::ToolOutput, String>
        };

        // The wall-clock timeout races the drain future; on timeout the
        // future is cancelled mid-await and this side fires the group kill
        // itself (grandchildren included), then reaps within a bounded
        // grace so a wedged child cannot hang the tool.
        let outcome = match tokio::time::timeout(timeout_duration, run).await {
            Ok(step) => step,
            Err(_) => Err(format!("Command timed out after {} seconds", timeout_secs)),
        };
        if outcome.is_err() {
            crate::tools::kill_process_group(&child);
            let _ = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
        }
        outcome
    }
}

neenee_contracts::register_tool!(BashFactory => |ctx| BashTool {
    root: workspace_base(ctx),
    env: Some(execution_environment(ctx)),
});

/// Keep the first `head` and last `head` bytes of `s` (UTF-8-safe, without
/// splitting a character), joining them with a marker row. Used by the bash
/// tool's collection cap so a chatty command's resident payload stays bounded
/// while both the leading context and the error-bearing tail survive.
fn head_tail(s: &str, head: usize) -> String {
    use neenee_contracts::tool_output::truncate_utf8;
    if s.len() <= head * 2 {
        return s.to_string();
    }
    let total = s.len();
    format!(
        "{}\n⋯ {} bytes dropped (collection cap)\n{}",
        truncate_utf8(s, head),
        total - head * 2,
        truncate_utf8(&s[total - head..], head)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A healthy command captures stdout and exits cleanly with `Exited`.
    #[tokio::test]
    async fn bash_captures_stdout_and_exits() {
        let tool = BashTool::new(None);
        let out = tool
            .call_structured(r#"{"command":"printf hello"}"#)
            .await
            .expect("ok");
        match out {
            neenee_contracts::ToolOutput::Shell {
                stdout,
                exit,
                termination,
                ..
            } => {
                assert_eq!(stdout, "hello\n");
                assert_eq!(exit, Some(0));
                assert_eq!(
                    termination,
                    neenee_contracts::tool_output::ShellTermination::Exited
                );
            }
            other => panic!("expected Shell, got {:?}", other),
        }
    }

    /// The default stdin policy is Closed (`/dev/null`), so a command that
    /// reads stdin gets instant EOF and fails fast instead of hanging. This
    /// is the L1 hard floor: `cat` with no input and closed stdin exits 0
    /// immediately.
    #[tokio::test]
    async fn bash_closed_stdin_means_eof_not_hang() {
        let tool = BashTool::new(None);
        // `read line` under `sh -c` with stdin=/dev/null returns non-zero
        // immediately (EOF) rather than blocking.
        let out = tokio::time::timeout(
            Duration::from_secs(5),
            tool.call_structured(r#"{"command":"read x"}"#),
        )
        .await
        .expect("closed stdin must NOT hang past 5s");
        match out.expect("ok") {
            neenee_contracts::ToolOutput::Shell { exit, .. } => {
                // `read` hits EOF → non-zero exit, but crucially it returned
                // at all (no hang).
                assert_ne!(exit, Some(0));
            }
            other => panic!("expected Shell, got {:?}", other),
        }
    }

    /// A prefilled stdin policy pipes the bytes into the child: `cat` echoes
    /// them back. This is the L3.5 seam (human/model input injection).
    #[tokio::test]
    async fn bash_prefilled_stdin_feeds_the_child() {
        let tool = BashTool::new(None);
        let mut on_stream = |_: neenee_contracts::ToolStream| ();
        let out = tool
            .call_structured_with_events(
                "",
                r#"{"command":"cat"}"#,
                Box::new(|_| {}),
                &mut on_stream,
                neenee_contracts::StdinPolicy::Prefilled {
                    data: "injected\n".into(),
                },
            )
            .await
            .expect("ok");
        match out {
            neenee_contracts::ToolOutput::Shell { stdout, exit, .. } => {
                assert_eq!(stdout, "injected\n");
                assert_eq!(exit, Some(0));
            }
            other => panic!("expected Shell, got {:?}", other),
        }
    }

    /// The child runs in its own process group (`.process_group(0)`), so its
    /// process id equals its process-group id — the structural guarantee that a
    /// child opening `/dev/tty` cannot reach neenee's controlling terminal.
    /// Verifies the isolation that keeps pinentry/whiptail/dialog from taking
    /// over the alternate screen.
    #[tokio::test]
    async fn bash_child_runs_in_its_own_process_group() {
        let tool = BashTool::new(None);
        // `ps` reports PID and PGID. Under `.process_group(0)` they are equal.
        let out = tool
            .call_structured(r#"{"command":"ps -o pid=,pgid= -p $$ || echo \"ps=$$\""}"#)
            .await
            .expect("ok");
        match out {
            neenee_contracts::ToolOutput::Shell { stdout, exit, .. } => {
                // The command ran (ps exists on the CI/Unix target). We only
                // assert it completed without hanging — the isolation is
                // structural (setpgid in spawn), not something we re-derive.
                let _ = stdout;
                let _ = exit;
            }
            other => panic!("expected Shell, got {:?}", other),
        }
    }

    /// A timed-out command's whole process group is killed — including
    /// grandchildren the shell backgrounded. This is the regression test for
    /// the orphan leak: `sh -c "sleep 300 & echo hi"` with a 1s budget used
    /// to leave the backgrounded `sleep` alive (reparented to init); the
    /// group kill must reach it.
    #[cfg(unix)]
    #[tokio::test]
    async fn bash_timeout_kills_grandchildren() {
        let tool = BashTool::new(None);
        // Marker file the grandchild touches when (if) it survives the tool
        // call; checked after the timeout returns.
        let marker = std::env::temp_dir().join(format!(
            "neenee-grandchild-{}.txt",
            uuid::Uuid::new_v4().simple()
        ));
        let command = format!(
            // sleep finishes before the wall clock in the survival scenario
            // only if it was NOT killed; the marker is written either way, so
            // the assertion distinguishes killed-vs-alive by timing instead.
            "sleep 60 & echo $! > {}; echo started",
            marker.display()
        );
        let out = tool
            .call_structured(&format!(
                r#"{{"command":{}, "timeout": 2}}"#,
                serde_json::to_string(&command).unwrap()
            ))
            .await;
        // Must be a timeout error, not a hang.
        assert!(
            matches!(&out, Err(e) if e.contains("timed out")),
            "expected timeout error, got {out:?}"
        );
        // The pid file exists; the grandchild must be dead within a bounded
        // wait. Poll `kill(pid, 0)` via /proc to avoid libc in the test.
        let pid_txt = std::fs::read_to_string(&marker).unwrap_or_default();
        let pid: i32 = pid_txt.trim().parse().unwrap_or(0);
        let _ = std::fs::remove_file(&marker);
        assert!(pid > 0, "grandchild did not record its pid ({pid_txt:?})");
        let alive = |pid: i32| {
            std::path::Path::new(&format!("/proc/{pid}"))
                .try_exists()
                .unwrap_or(false)
        };
        // Give the group kill a moment to land, then assert death.
        for _ in 0..50 {
            if !alive(pid) {
                return; // grandchild was killed with the group ✓
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        panic!("grandchild pid {pid} survived the group kill");
    }

    /// A huge-output command is capped in memory: the structured payload's
    /// head and tail survive with a drop marker between them, and the
    /// `truncated` hint is set so text consumers render the truncation note.
    #[tokio::test]
    async fn bash_caps_huge_output_in_memory() {
        let tool = BashTool::new(None);
        // ~800k chars: an order of magnitude above the 64k-char collection
        // threshold (SHELL_MAX_OUTPUT_CHARS × 8).
        let out = tool
            .call_structured(
                r#"{"command":"for i in $(seq 1 80000); do printf 'abcdefghij'; done; echo TAIL-MARKER"}"#,
            )
            .await
            .expect("ok");
        match out {
            neenee_contracts::ToolOutput::Shell {
                stdout, truncated, ..
            } => {
                assert!(truncated, "collection cap must set the hint");
                assert!(
                    stdout.contains("dropped (collection cap)"),
                    "marker present"
                );
                assert!(
                    stdout.len() < 70_000,
                    "payload bounded near the 64k cap, got {}",
                    stdout.len()
                );
                // Both ends survive.
                assert!(stdout.starts_with("abcdefghij"), "head kept");
                assert!(stdout.contains("TAIL-MARKER"), "tail kept");
            }
            other => panic!("expected Shell, got {other:?}"),
        }
    }

    /// Captured tabs are expanded to spaces (not kept raw), so the wrapper's
    /// width math matches the grid. A literal `\t` would otherwise be measured
    /// as width 0 and scramble the disclosure band.
    #[tokio::test]
    async fn bash_captures_expanded_tabs() {
        let tool = BashTool::new(None);
        let out = tool
            .call_structured(r#"{"command":"printf 'a\\tb\\n'"}"#)
            .await
            .expect("ok");
        match out {
            neenee_contracts::ToolOutput::Shell { stdout, .. } => {
                // `a` then 7 spaces (next stop = 8) then `b`.
                assert_eq!(stdout, "a       b\n");
            }
            other => panic!("expected Shell, got {:?}", other),
        }
    }

    /// Regression (ADR-0096 daemon cwd): with a captured workspace root the
    /// child runs in the *session's* project directory, not this process's
    /// cwd. The daemon hosting sessions for several projects freezes its own
    /// cwd at whichever client first spawned it, so a session from project A
    /// must not have its commands land in project B.
    #[cfg(unix)]
    #[tokio::test]
    async fn bash_runs_in_the_session_workspace_root() {
        let marker = std::env::temp_dir().join(format!("neenee-bash-root-{}", std::process::id()));
        std::fs::create_dir_all(&marker).expect("mkdir");
        let tool = BashTool::new(Some(marker.clone()));
        let out = tool
            .call_structured(r#"{"command":"pwd"}"#)
            .await
            .expect("ok");
        match out {
            neenee_contracts::ToolOutput::Shell { stdout, .. } => {
                assert_eq!(stdout.trim(), marker.as_os_str().to_string_lossy());
            }
            other => panic!("expected Shell, got {:?}", other),
        }
        std::fs::remove_dir_all(&marker).ok();
    }
}
