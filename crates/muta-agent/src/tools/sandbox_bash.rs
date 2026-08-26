use async_trait::async_trait;
use muta_contracts::Tool;
use serde_json::json;
use std::sync::Arc;
use tokio::time::Duration;

use crate::tools::helpers::{
    WorkspaceBase, env_from_root, execution_environment, json_string, workspace_base,
};

/// Execute a command in an isolated workspace sandbox container.
///
/// Encapsulates physical workspace containment as a tool, allowing presets
/// like Code Analysis (`MASTER_CODE_ANALYST`) to execute tests and probes in
/// containment without access to the host environment.
pub struct SandboxBashTool {
    pub(crate) root: WorkspaceBase,
    pub(crate) env: Option<Arc<dyn muta_contracts::ExecutionEnvironment>>,
}

impl SandboxBashTool {
    pub fn new(root: Option<std::path::PathBuf>) -> Self {
        Self { root, env: None }
    }

    pub fn with_env(env: Arc<dyn muta_contracts::ExecutionEnvironment>) -> Self {
        let root = Some(env.workspace_root().to_path_buf());
        Self {
            root,
            env: Some(env),
        }
    }
}

#[async_trait]
impl Tool for SandboxBashTool {
    fn name(&self) -> &str {
        "sandbox_bash"
    }

    fn description(&self) -> &str {
        "Execute a shell command inside an isolated workspace sandbox container. Use for basic functional tests, cargo metadata inspection, and contained checks. File access is strictly confined to workspace roots and cannot read or modify host files outside."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "The shell command to execute inside the sandbox container" },
                "timeout": { "type": "integer", "description": "Overall timeout in seconds (default 30). Output idle for 10s is killed early." }
            },
            "required": ["command"]
        })
    }

    fn scope_target(&self, arguments: &str) -> muta_contracts::ScopeTarget {
        muta_contracts::ScopeTarget::Command(json_string(arguments, "command"))
    }

    fn hazard_level(&self) -> muta_contracts::HazardLevel {
        muta_contracts::HazardLevel::CommandExecution
    }

    fn permission_submission(&self, arguments: &str) -> Option<muta_contracts::ToolPermissionSubmission> {
        let command = json_string(arguments, "command");
        let first_word = command.split_whitespace().next().unwrap_or("sh");
        Some(muta_contracts::ToolPermissionSubmission {
            hazard_level: muta_contracts::HazardLevel::CommandExecution,
            label: format!("Execute in sandbox: `{}`", if command.len() > 50 { format!("{}...", &command[..47]) } else { command.clone() }),
            description: format!("Runs command `{command}` inside isolated bubblewrap container sandbox."),
            scope: command.clone(),
            payload: muta_contracts::ToolPermissionPayload::Command {
                command: command.clone(),
                cwd: None,
                kill_spec: muta_contracts::ProcessKillSpec {
                    command: first_word.to_string(),
                    process_group_killable: true,
                    pkill_target: format!("pkill -f '{first_word}'"),
                    cwd: None,
                },
            },
        })
    }


    async fn call(&self, arguments: &str) -> Result<String, String> {
        self.call_structured(arguments).await.map(|o| o.to_text())
    }

    async fn call_structured(&self, arguments: &str) -> Result<muta_contracts::ToolOutput, String> {
        self.call_structured_with_events(
            "",
            arguments,
            Box::new(|_| {}),
            &mut |_| {},
            muta_contracts::StdinPolicy::default(),
        )
        .await
    }

    async fn call_structured_with_events<'a>(
        &self,
        _call_id: &str,
        arguments: &str,
        _on_event: Box<dyn FnMut(muta_contracts::RunnerEvent) + Send + 'a>,
        on_stream: &mut (dyn FnMut(muta_contracts::ToolStream) + Send + 'a),
        stdin_policy: muta_contracts::StdinPolicy,
    ) -> Result<muta_contracts::ToolOutput, String> {
        use muta_contracts::tool_output::{
            ShellLine, ShellStream, normalize_carriage_returns, strip_ansi,
        };
        use tokio::io::{AsyncBufReadExt, BufReader};

        if !muta_platform::workspace_sandbox::available() {
            return Err(
                "Workspace sandbox execution (sandbox_bash) is unavailable on this host platform."
                    .to_string(),
            );
        }

        const SHELL_COLLECT_MAX_CHARS: usize =
            muta_contracts::tool_output::SHELL_MAX_OUTPUT_CHARS * 8;
        const SHELL_COLLECT_MAX_LINES: usize = 5_000;

        let args: serde_json::Value =
            serde_json::from_str(arguments).map_err(|e| format!("Invalid JSON: {e}"))?;
        let command = args["command"].as_str().ok_or("Missing 'command'")?;
        let timeout_secs = args["timeout"].as_u64().unwrap_or(30);
        let timeout_duration = Duration::from_secs(timeout_secs);

        let stdin_bytes = match &stdin_policy {
            muta_contracts::StdinPolicy::Closed => None,
            muta_contracts::StdinPolicy::Prefilled { data } => Some(data.clone()),
        };
        let stdin_stdio = if stdin_bytes.is_some() {
            std::process::Stdio::piped()
        } else {
            std::process::Stdio::null()
        };

        let env = self
            .env
            .clone()
            .unwrap_or_else(|| env_from_root(&self.root));

        let mut invocation = muta_platform::workspace_sandbox::shell_with_roots(
            command,
            env.workspace_root(),
            env.additional_roots(),
            muta_platform::workspace_sandbox::WorkspaceAccess::ReadWrite,
            muta_platform::workspace_sandbox::NetworkAccess::Disabled,
        )?;

        invocation
            .kill_on_drop(true)
            .stdin(stdin_stdio)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        invocation.current_dir(env.workspace_root());

        let (mut child, process_tree) = muta_platform::process::spawn_owned(&mut invocation)
            .map_err(|e| format!("Failed to spawn sandbox process: {e}"))?;

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
            .ok_or("failed to capture sandbox stdout")?;
        let stderr = child
            .stderr
            .take()
            .ok_or("failed to capture sandbox stderr")?;

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<(ShellStream, String)>();

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
        drop(tx);

        let idle_budget = Duration::from_secs(10);
        let timeout_deadline = tokio::time::Instant::now() + timeout_duration;

        let mut lines: Vec<ShellLine> = Vec::new();
        let mut stdout_buf = String::new();
        let mut stderr_buf = String::new();
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
                msg = rx.recv() => {
                    match msg {
                        Some((stream, text)) => {
                            match stream {
                                ShellStream::Out => {
                                    stdout_buf.push_str(&text);
                                    stdout_buf.push('\n');
                                    on_stream(muta_contracts::ToolStream::Stdout(
                                        format!("{text}\n"),
                                    ));
                                }
                                ShellStream::Err => {
                                    stderr_buf.push_str(&text);
                                    stderr_buf.push('\n');
                                    on_stream(muta_contracts::ToolStream::Stderr(
                                        format!("{text}\n"),
                                    ));
                                }
                            }
                            lines.push(ShellLine { stream, text });
                        }
                        None => break,
                    }
                }
            }
        }

        if timed_out || idle_blocked {
            let _ = process_tree.terminate();
            stdout_task.abort();
            stderr_task.abort();
            while let Ok((stream, text)) = rx.try_recv() {
                match stream {
                    ShellStream::Out => {
                        stdout_buf.push_str(&text);
                        stdout_buf.push('\n');
                    }
                    ShellStream::Err => {
                        stderr_buf.push_str(&text);
                        stderr_buf.push('\n');
                    }
                }
                lines.push(ShellLine { stream, text });
            }
            let _ = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
        } else {
            let _ = stdout_task.await;
            let _ = stderr_task.await;
        }

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

        let truncated = collection_truncated
            || muta_contracts::tool_output::shell_inner_text(&stdout_buf, &stderr_buf, exit)
                .len()
                > muta_contracts::tool_output::SHELL_MAX_OUTPUT_CHARS;

        Ok(muta_contracts::ToolOutput::Shell {
            command: command.to_string(),
            stdout: stdout_buf,
            stderr: stderr_buf,
            lines,
            exit,
            truncated,
            termination,
        })
    }
}

muta_contracts::register_tool!(SandboxBashFactory => |ctx| SandboxBashTool {
    root: workspace_base(ctx),
    env: Some(execution_environment(ctx)),
});

fn head_tail(s: &str, head: usize) -> String {
    use muta_contracts::tool_output::truncate_utf8;
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

    #[tokio::test]
    async fn sandbox_bash_tool_metadata() {
        let tool = SandboxBashTool::new(None);
        assert_eq!(tool.name(), "sandbox_bash");
        assert!(!tool.description().is_empty());
        assert_eq!(
            tool.scope_target(r#"{"command":"ls"}"#),
            muta_contracts::ScopeTarget::Command("ls".to_string())
        );
    }
}
