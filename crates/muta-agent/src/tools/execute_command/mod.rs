mod episodic;
pub mod persistent;
pub mod pipes;

#[cfg(test)]
mod tests;

#[allow(unused_imports)]
pub use episodic::workspace_sandbox_shell;
#[allow(unused_imports)]
pub use persistent::PersistentTerminalSession;

use async_trait::async_trait;
use muta_contracts::Tool;
use muta_tool_derive::ToolSchema;
use serde::Deserialize;
use tokio::time::Duration;

use crate::tools::helpers::{
    WorkspaceBase, env_from_root, execution_environment, json_string, workspace_base,
};

#[derive(ToolSchema, Deserialize)]
struct ExecuteCommandArgs {
    #[tool(desc = "The shell command to execute")]
    command: String,
    #[tool(
        desc = "Overall timeout in seconds (default 1800 = 30 minutes). A command producing no output for timeout/3 (min 5s, max 480s) is killed early as a blocked-command guard."
    )]
    timeout: Option<u64>,
    #[tool(
        desc = "Optional persistent terminal session identifier to reuse environment variables, cwd, and shell state across commands."
    )]
    terminal_id: Option<String>,
    #[tool(desc = "Set to true to run in a persistent terminal session.")]
    run_persistent: Option<bool>,
}

#[allow(dead_code)]
#[derive(ToolSchema, Deserialize)]
struct WorkspaceExecuteCommandArgs {
    #[tool(desc = "The shell command to execute inside the workspace sandbox")]
    command: String,
    #[tool(
        desc = "Overall timeout in seconds (default 1800 = 30 minutes). A command producing no output for timeout/3 (min 5s, max 480s) is killed early as a blocked-command guard."
    )]
    timeout: Option<u64>,
}

/// Execute a command in a non-interactive shell.
///
/// Commands run in the session's workspace root (captured at factory time),
/// not the daemon process's cwd — under the unified daemon (ADR-0096) those
/// differ whenever the daemon was first spawned from another project.
pub struct ExecuteCommandTool {
    pub(crate) root: WorkspaceBase,
    pub(crate) env: Option<std::sync::Arc<dyn muta_contracts::ExecutionEnvironment>>,
    workspace_sandbox: bool,
}

impl ExecuteCommandTool {
    /// Build the default host-command variant against a workspace root.
    /// Runtime uses this for the `!`-prefix shell path, which bypasses the
    /// factory-based toolset assembly but must still run in the session's
    /// project (not the daemon's process cwd, ADR-0096).
    pub fn new(root: Option<std::path::PathBuf>) -> Self {
        Self {
            root,
            env: None,
            workspace_sandbox: false,
        }
    }

    /// Build the shell tool backed by a custom execution environment.
    pub fn with_env(env: std::sync::Arc<dyn muta_contracts::ExecutionEnvironment>) -> Self {
        let root = Some(env.workspace_root().to_path_buf());
        Self {
            root,
            env: Some(env),
            workspace_sandbox: false,
        }
    }

    /// Build the workspace-contained variant. It shares the same
    /// model-facing capability name; agent presets select it by variant id.
    pub fn workspace_with_env(
        env: std::sync::Arc<dyn muta_contracts::ExecutionEnvironment>,
    ) -> Self {
        let root = Some(env.workspace_root().to_path_buf());
        Self {
            root,
            env: Some(env),
            workspace_sandbox: true,
        }
    }

    fn shell_isolation(&self) -> muta_contracts::ShellIsolation {
        if self.workspace_sandbox {
            muta_contracts::ShellIsolation::Workspace
        } else {
            self.env
                .as_ref()
                .map(|env| env.shell_isolation())
                .unwrap_or(muta_contracts::ShellIsolation::Host)
        }
    }
}

#[async_trait]
impl Tool for ExecuteCommandTool {
    fn name(&self) -> &str {
        "run_command"
    }
    fn variant(&self) -> &str {
        if self.workspace_sandbox {
            "workspace"
        } else {
            "default"
        }
    }
    fn is_available(&self) -> bool {
        self.shell_isolation() != muta_contracts::ShellIsolation::Workspace
            || muta_platform::workspace_sandbox::available()
    }
    /// The command tool's primary purpose is execution, not workspace
    /// mutation — so it sits in the `Execute` tier between pure reads and
    /// file-writing tools. The broker still gates it (`Execute > Read`). See
    /// ADR-0012.
    fn description(&self) -> &str {
        if self.workspace_sandbox {
            "Execute a shell command inside the isolated workspace. Use for builds, tests, metadata inspection, and contained checks. Host files outside the admitted workspace roots and network access are unavailable."
        } else {
            "Execute a shell command. Use for build, test, git, or system commands. Supports persistent sessions via run_persistent or terminal_id."
        }
    }
    fn parameters(&self) -> serde_json::Value {
        if self.workspace_sandbox {
            WorkspaceExecuteCommandArgs::parameters_schema()
        } else {
            ExecuteCommandArgs::parameters_schema()
        }
    }
    fn scope_target(&self, arguments: &str) -> muta_contracts::ScopeTarget {
        muta_contracts::ScopeTarget::Command(json_string(arguments, "command"))
    }
    fn hazard_level(&self) -> muta_contracts::HazardLevel {
        muta_contracts::HazardLevel::CommandExecution
    }
    fn permission_submission(
        &self,
        arguments: &str,
    ) -> Option<muta_contracts::ToolPermissionSubmission> {
        let command = json_string(arguments, "command");
        let first_word = command.split_whitespace().next().unwrap_or("sh");
        let sandboxed = self.shell_isolation() == muta_contracts::ShellIsolation::Workspace;
        Some(muta_contracts::ToolPermissionSubmission {
            hazard_level: muta_contracts::HazardLevel::CommandExecution,
            label: format!(
                "Execute{}: `{}`",
                if sandboxed {
                    " in workspace"
                } else {
                    " command"
                },
                if command.len() > 50 {
                    format!("{}...", &command[..47])
                } else {
                    command.clone()
                }
            ),
            description: if sandboxed {
                format!(
                    "Runs command `{command}` inside the isolated workspace with network access disabled."
                )
            } else {
                format!(
                    "Runs host shell command `{command}`. May modify system state or execute arbitrary binaries."
                )
            },
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
        let args: ExecuteCommandArgs =
            serde_json::from_str(arguments).map_err(|e| format!("Invalid JSON: {}", e))?;
        let timeout_secs = args.timeout.unwrap_or(1800);
        let timeout_duration = Duration::from_secs(timeout_secs);

        let terminal_id = args.terminal_id.as_deref().or_else(|| {
            if args.run_persistent == Some(true) {
                Some("default")
            } else {
                None
            }
        });

        if let Some(term_id) = terminal_id {
            if self.shell_isolation() == muta_contracts::ShellIsolation::Workspace {
                return Err(
                    "Persistent terminal sessions are disabled in the workspace sandbox; run a non-persistent command instead."
                        .to_string(),
                );
            }
            let env = self
                .env
                .clone()
                .unwrap_or_else(|| env_from_root(&self.root));
            let root = env.workspace_root().to_path_buf();
            return persistent::run_persistent_command(
                &root,
                term_id,
                &args.command,
                timeout_duration,
                on_stream,
            )
            .await;
        }

        let env = self
            .env
            .clone()
            .unwrap_or_else(|| env_from_root(&self.root));
        episodic::run_episodic_command(
            &args.command,
            timeout_duration,
            self.shell_isolation(),
            env,
            stdin_policy,
            on_stream,
        )
        .await
    }
}

muta_contracts::register_tool!(ExecuteCommandFactory => |ctx| ExecuteCommandTool {
    root: workspace_base(ctx),
    env: Some(execution_environment(ctx)),
    workspace_sandbox: false,
});

muta_contracts::register_tool!(WorkspaceExecuteCommandFactory => |ctx| {
    ExecuteCommandTool::workspace_with_env(execution_environment(ctx))
});
